// Copyright 2022 Parity Technologies (UK) Ltd.
// This file is part of polkadot-introspector.
//
// polkadot-introspector is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// polkadot-introspector is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with polkadot-introspector.  If not, see <http://www.gnu.org/licenses/>.
//

use crate::{
	api::decode::{
		DecodedBabeEpoch, DecodedDispute, babe_current_slot_key, decode_account_id, decode_account_keys,
		decode_availability_cores, decode_babe_epoch, decode_candidate_events, decode_claim_queue, decode_disputes,
		decode_hrmp_channel, decode_hrmp_channel_digests, decode_para_ids, decode_parainherent,
		decode_queued_authority_discovery_keys, decode_session_index, decode_slot, decode_timestamp,
		decode_validator_groups, hrmp_channel_digests_key, hrmp_channels_key, hrmp_egress_channels_index_key,
		para_session_account_keys_key, queued_keys_key, session_key_owner_key, timestamp_now_key,
	},
	chain_events::SubxtCandidateEvent,
	types::{
		AccountId32, BlockNumber, ClaimQueue, CoreOccupied, H256, Header, InherentData, QueuedKeys, Slot,
		SubxtHrmpChannel, Timestamp, ValidatorIndex, header_hash,
	},
};
use clap::ValueEnum;
use futures::{Stream, StreamExt, stream, stream::BoxStream};
use jsonrpsee::{
	core::{
		client::{ClientT, SubscriptionClientT},
		params::ArrayParams,
		traits::ToRpcParams,
	},
	rpc_params,
	ws_client::{WsClient, WsClientBuilder},
};
use serde::de::DeserializeOwned;
use std::{
	collections::BTreeMap,
	fmt,
	pin::Pin,
	sync::{Arc, Mutex},
	task::{Context, Poll},
	time::Duration,
};
use subxt_lightclient::{LightClient, LightClientRpc, LightClientRpcError};
use thiserror::Error;

/// A stream of `(header, block hash)` pairs.
pub struct HeaderStream(BoxStream<'static, Result<(Header, H256), ClientError>>);

impl Stream for HeaderStream {
	type Item = Result<(Header, H256), ClientError>;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		self.0.poll_next_unpin(cx)
	}
}

impl fmt::Debug for HeaderStream {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("HeaderStream")
	}
}

/// Error from the RPC client: transport failures, undecodable responses, or missing data.
#[derive(Debug, Error)]
pub enum ClientError {
	#[error("RPC error: {0}")]
	Rpc(#[from] jsonrpsee::core::ClientError),
	#[error("Light client error: {0}")]
	Light(#[from] LightClientRpcError),
	#[error("{0}")]
	Other(String),
}

/// The `chain_getBlock` response.
#[derive(serde_derive::Deserialize)]
struct SignedBlock {
	block: BlockBody,
}

#[derive(serde_derive::Deserialize)]
struct BlockBody {
	extrinsics: Vec<String>,
}

/// How to talk to the chain: a full node over websocket, or an embedded smoldot light client.
#[derive(strum::Display, Debug, Clone, Copy, ValueEnum, Default)]
pub enum ApiClientMode {
	#[default]
	RPC,
	Light,
}

#[derive(Clone)]
enum Transport {
	Ws(Arc<WsClient>),
	Light(LightClientRpc),
}

impl Transport {
	async fn request<T: DeserializeOwned>(&self, method: &str, params: ArrayParams) -> Result<T, ClientError> {
		match self {
			Self::Ws(client) => Ok(client.request(method, params).await?),
			Self::Light(client) => {
				let params = params.to_rpc_params().map_err(|e| ClientError::Other(e.to_string()))?;
				let raw = client.request(method.to_string(), params).await?;
				serde_json::from_str(raw.get())
					.map_err(|e| ClientError::Other(format!("invalid {method} response: {e}")))
			},
		}
	}

	async fn subscribe<T: DeserializeOwned + Send + 'static>(
		&self,
		method: &str,
		unsubscribe: &str,
		params: ArrayParams,
	) -> Result<BoxStream<'static, Result<T, ClientError>>, ClientError> {
		match self {
			Self::Ws(client) => {
				let subscription = client.subscribe(method, params, unsubscribe).await?;
				Ok(subscription
					.map(|item| item.map_err(|e| jsonrpsee::core::ClientError::ParseError(e).into()))
					.boxed())
			},
			Self::Light(client) => {
				let params = params.to_rpc_params().map_err(|e| ClientError::Other(e.to_string()))?;
				let subscription = client.subscribe(method.to_string(), params, unsubscribe.to_string()).await?;
				Ok(subscription
					.map(|item| {
						let raw = item.map_err(|e| ClientError::Light(LightClientRpcError::JsonRpcError(e)))?;
						serde_json::from_str(raw.get()).map_err(|e| ClientError::Other(e.to_string()))
					})
					.boxed())
			},
		}
	}
}

#[derive(Clone)]
pub struct ApiClient {
	client: Transport,
}

impl ApiClient {
	/// Requests an optional hex-encoded result.
	async fn request_hex(&self, method: &str, params: ArrayParams) -> Result<Option<Vec<u8>>, ClientError> {
		let hex: Option<String> = self.client.request(method, params).await?;
		hex.map(|raw| {
			hex::decode(raw.trim_start_matches("0x"))
				.map_err(|e| ClientError::Other(format!("invalid hex from {method}: {e}")))
		})
		.transpose()
	}

	/// Fetches a header via `chain_getHeader`, returning `None` when the node has no header for the
	/// hash (e.g. a pruned block in historical mode).
	async fn maybe_header_via_rpc(&self, hash: H256) -> Result<Option<Header>, ClientError> {
		self.client.request("chain_getHeader", rpc_params![hash]).await
	}

	/// Fetches a header via `chain_getHeader`, failing when the node has none for the hash.
	async fn header_via_rpc(&self, hash: H256) -> Result<Header, ClientError> {
		self.maybe_header_via_rpc(hash)
			.await?
			.ok_or_else(|| ClientError::Other(format!("header {hash:?} not found via chain_getHeader")))
	}

	/// Resolves `None` to the latest finalized block hash
	async fn resolve_hash(&self, maybe_hash: Option<H256>) -> Result<H256, ClientError> {
		match maybe_hash {
			Some(hash) => Ok(hash),
			None => Ok(self.client.request("chain_getFinalizedHead", rpc_params![]).await?),
		}
	}

	/// Resolves a header and its hash via `chain_getHeader`, at the latest finalized block for `None`.
	async fn resolve_header(&self, maybe_hash: Option<H256>) -> Result<(H256, Header), ClientError> {
		let hash = self.resolve_hash(maybe_hash).await?;
		Ok((hash, self.header_via_rpc(hash).await?))
	}

	/// Reads a raw storage value via `state_getStorage` and decodes it with the given metadata-free
	/// decoder.
	async fn storage_via_rpc<R>(
		&self,
		hash: H256,
		key: &[u8],
		read: &str,
		decode: impl Fn(&[u8]) -> color_eyre::Result<R>,
	) -> Result<Option<R>, ClientError> {
		self.request_hex("state_getStorage", rpc_params![format!("0x{}", hex::encode(key)), hash])
			.await?
			.map(|bytes| decode(&bytes))
			.transpose()
			.map_err(|e| ClientError::Other(format!("Failed to decode {read} (metadata-free): {e}")))
	}

	/// Calls a runtime API via `state_call` and decodes the result with the given metadata-free decoder.
	async fn state_call_decoded<R>(
		&self,
		hash: H256,
		method: &str,
		decode: impl Fn(&[u8]) -> color_eyre::Result<R>,
	) -> Result<R, ClientError> {
		let bytes = self
			.request_hex("state_call", rpc_params![method, "0x", hash])
			.await?
			.ok_or_else(|| ClientError::Other(format!("empty response from {method}")))?;
		decode(&bytes).map_err(|e| ClientError::Other(format!("Failed to decode {method}: {e}")))
	}

	async fn get_hrmp_egress_channels_index(
		client: ApiClient,
		block_hash: H256,
		para_id: u32,
	) -> Result<Vec<u32>, ClientError> {
		let key = hrmp_egress_channels_index_key(para_id);
		Ok(client
			.storage_via_rpc(block_hash, &key, "HrmpEgressChannelsIndex", decode_para_ids)
			.await?
			.unwrap_or_default())
	}

	async fn get_hrmp_channel_digests(
		client: ApiClient,
		block_hash: H256,
		para_id: u32,
	) -> Result<Vec<(u32, Vec<u32>)>, ClientError> {
		let key = hrmp_channel_digests_key(para_id);
		Ok(client
			.storage_via_rpc(block_hash, &key, "HrmpChannelDigests", decode_hrmp_channel_digests)
			.await?
			.unwrap_or_default())
	}

	async fn get_hrmp_channels(
		client: ApiClient,
		block_hash: H256,
		sender: u32,
		recipient: u32,
	) -> Result<Option<(u32, u32, SubxtHrmpChannel)>, ClientError> {
		let key = hrmp_channels_key(sender, recipient);
		let channel = client
			.storage_via_rpc(block_hash, &key, "HrmpChannels", decode_hrmp_channel)
			.await?;

		Ok(channel.map(|channel| (sender, recipient, channel)))
	}

	async fn get_inbound_hrmp_channel_pairs(
		&self,
		block_hash: H256,
		para_ids: Vec<u32>,
	) -> color_eyre::Result<Vec<(u32, u32)>, ClientError> {
		let inbound_ids_fut = para_ids
			.iter()
			.map(|&para_id| tokio::spawn(Self::get_hrmp_channel_digests(self.clone(), block_hash, para_id)));
		let inbound_ids: Vec<_> = join_requests(inbound_ids_fut)
			.await?
			.iter()
			.map(|v| -> Vec<_> { v.iter().flat_map(|(_, v)| v).cloned().collect() })
			.collect();

		Ok(inbound_ids
			.into_iter()
			.zip(para_ids)
			.flat_map(|(ids, para_id)| -> Vec<_> { ids.into_iter().map(|sender| (sender, para_id)).collect() })
			.collect())
	}

	async fn get_outbound_hrmp_channel_pairs(
		&self,
		block_hash: H256,
		para_ids: Vec<u32>,
	) -> color_eyre::Result<Vec<(u32, u32)>, ClientError> {
		let outbound_ids_fut = para_ids
			.iter()
			.map(|&para_id| tokio::spawn(Self::get_hrmp_egress_channels_index(self.clone(), block_hash, para_id)));
		let outbound_ids: Vec<Vec<u32>> = join_requests(outbound_ids_fut).await?;

		Ok(para_ids
			.into_iter()
			.zip(outbound_ids)
			.flat_map(|(para_id, ids)| -> Vec<_> { ids.into_iter().map(|recipient| (para_id, recipient)).collect() })
			.collect())
	}
}

impl ApiClient {
	pub async fn get_head(&self, maybe_hash: Option<H256>) -> Result<Option<Header>, ClientError> {
		match maybe_hash {
			Some(hash) => self.maybe_header_via_rpc(hash).await,
			None => Ok(Some(self.resolve_header(None).await?.1)),
		}
	}

	pub async fn get_block_number(&self, maybe_hash: Option<H256>) -> Result<BlockNumber, ClientError> {
		Ok(self.resolve_header(maybe_hash).await?.1.number)
	}

	pub async fn get_block_ts(&self, hash: H256) -> Result<Option<Timestamp>, ClientError> {
		self.storage_via_rpc(hash, &timestamp_now_key(), "Timestamp.Now", decode_timestamp)
			.await
	}

	/// Reads candidate (backed/included/timed-out) events for a block through the
	/// `ParachainHost_candidate_events` runtime call and decodes them without metadata.
	pub async fn get_candidate_events(&self, hash: H256) -> Result<Vec<SubxtCandidateEvent>, ClientError> {
		self.state_call_decoded(hash, "ParachainHost_candidate_events", decode_candidate_events)
			.await
	}

	/// Reads the recent disputes for a block through the `ParachainHost_disputes` runtime call and
	/// decodes them without metadata.
	pub async fn get_disputes(&self, hash: H256) -> Result<Vec<DecodedDispute>, ClientError> {
		self.state_call_decoded(hash, "ParachainHost_disputes", decode_disputes).await
	}

	/// Reads the current Babe epoch through the `BabeApi_current_epoch` runtime call, decoded without
	/// metadata. Primary source of the Babe randomness and authorities.
	async fn babe_current_epoch(&self, hash: H256) -> Result<DecodedBabeEpoch, ClientError> {
		self.state_call_decoded(hash, "BabeApi_current_epoch", decode_babe_epoch).await
	}

	pub async fn get_babe_randomness(&self, hash: H256) -> Result<[u8; 32], ClientError> {
		Ok(self.babe_current_epoch(hash).await?.randomness)
	}

	pub async fn get_babe_authorities(&self, hash: H256) -> Result<Vec<([u8; 32], u64)>, ClientError> {
		Ok(self.babe_current_epoch(hash).await?.authorities)
	}

	pub async fn get_babe_current_slot(&self, hash: H256) -> Result<Option<Slot>, ClientError> {
		self.storage_via_rpc(hash, &babe_current_slot_key(), "Babe.CurrentSlot", decode_slot)
			.await
	}

	pub async fn get_babe_key_owner(&self, hash: H256, public: &[u8]) -> Result<Option<AccountId32>, ClientError> {
		let key = session_key_owner_key(*b"babe", public);
		self.storage_via_rpc(hash, &key, "Session.KeyOwner", decode_account_id).await
	}

	pub async fn get_occupied_cores(&self, hash: H256) -> Result<Vec<CoreOccupied>, ClientError> {
		self.state_call_decoded(hash, "ParachainHost_availability_cores", decode_availability_cores)
			.await
	}

	pub async fn get_claim_queue(&self, hash: H256) -> Result<ClaimQueue, ClientError> {
		self.state_call_decoded(hash, "ParachainHost_claim_queue", decode_claim_queue)
			.await
	}

	pub async fn get_backing_groups(&self, hash: H256) -> Result<Vec<Vec<ValidatorIndex>>, ClientError> {
		let groups = self
			.state_call_decoded(hash, "ParachainHost_validator_groups", decode_validator_groups)
			.await?;

		Ok(groups
			.into_iter()
			.map(|group| group.into_iter().map(ValidatorIndex).collect())
			.collect())
	}

	pub async fn get_session_index(&self, hash: H256) -> Result<u32, ClientError> {
		self.state_call_decoded(hash, "ParachainHost_session_index_for_child", decode_session_index)
			.await
	}

	pub async fn get_session_index_now(&self) -> Result<u32, ClientError> {
		let hash = self.resolve_hash(None).await?;
		self.get_session_index(hash).await
	}

	pub async fn get_session_account_keys(
		&self,
		session_index: u32,
		maybe_hash: Option<H256>,
	) -> Result<Option<Vec<AccountId32>>, ClientError> {
		let hash = self.resolve_hash(maybe_hash).await?;
		let key = para_session_account_keys_key(session_index);
		self.storage_via_rpc(hash, &key, "account_keys", decode_account_keys).await
	}

	pub async fn get_session_queued_keys(&self, maybe_hash: Option<H256>) -> Result<Option<QueuedKeys>, ClientError> {
		let hash = self.resolve_hash(maybe_hash).await?;
		self.storage_via_rpc(hash, &queued_keys_key(), "Session.QueuedKeys", decode_queued_authority_discovery_keys)
			.await
	}

	pub async fn get_inbound_outbound_hrmp_channels(
		&self,
		block_hash: H256,
		para_ids: Vec<u32>,
	) -> color_eyre::Result<Vec<(u32, BTreeMap<u32, SubxtHrmpChannel>, BTreeMap<u32, SubxtHrmpChannel>)>, ClientError>
	{
		let inbound_pairs = self.get_inbound_hrmp_channel_pairs(block_hash, para_ids.clone()).await?;
		let inbound_channels_fut = inbound_pairs.iter().map(|(sender, para_id)| {
			tokio::spawn(Self::get_hrmp_channels(self.clone(), block_hash, *sender, *para_id))
		});
		let inbound_channels: Vec<_> = join_requests(inbound_channels_fut).await?.into_iter().flatten().collect();

		let mut inbound_by_para_id: BTreeMap<u32, BTreeMap<u32, SubxtHrmpChannel>> = BTreeMap::new();
		for (sender, para_id, channel) in inbound_channels {
			let channels = inbound_by_para_id.entry(para_id).or_default();
			channels.insert(sender, channel);
		}

		let outbound_pairs = self.get_outbound_hrmp_channel_pairs(block_hash, para_ids.clone()).await?;
		let outbound_channels_fut = outbound_pairs.iter().map(|(para_id, recipient)| {
			tokio::spawn(Self::get_hrmp_channels(self.clone(), block_hash, *para_id, *recipient))
		});
		let outbound_channels: Vec<_> = join_requests(outbound_channels_fut).await?.into_iter().flatten().collect();

		let mut outbound_by_para_id: BTreeMap<u32, BTreeMap<u32, SubxtHrmpChannel>> = BTreeMap::new();
		for (para_id, recipient, channel) in outbound_channels {
			let channels = outbound_by_para_id.entry(para_id).or_default();
			channels.insert(recipient, channel);
		}

		Ok(para_ids
			.into_iter()
			.map(|para_id| {
				(
					para_id,
					inbound_by_para_id.get(&para_id).cloned().unwrap_or_default(),
					outbound_by_para_id.get(&para_id).cloned().unwrap_or_default(),
				)
			})
			.collect())
	}

	pub async fn extract_parainherent(&self, maybe_hash: Option<H256>) -> Result<InherentData, ClientError> {
		let hash = self.resolve_hash(maybe_hash).await?;
		let block: SignedBlock = self
			.client
			.request::<Option<_>>("chain_getBlock", rpc_params![hash])
			.await?
			.ok_or_else(|| ClientError::Other(format!("block {hash:?} not found via chain_getBlock")))?;
		let para_inherent = block
			.block
			.extrinsics
			.get(1)
			.ok_or_else(|| ClientError::Other("`ParaInherent` extrinsic missing at index #1".to_string()))?;
		let para_inherent = hex::decode(para_inherent.trim_start_matches("0x"))
			.map_err(|e| ClientError::Other(format!("invalid ParaInherent extrinsic hex: {e}")))?;
		decode_parainherent(&para_inherent)
			.map_err(|e| ClientError::Other(format!("Failed to decode ParaInherent: {e}")))
	}

	// We need it only for the historical mode to convert block numbers into their hashes
	pub async fn legacy_get_block_hash(
		&self,
		maybe_block_number: Option<BlockNumber>,
	) -> Result<Option<H256>, ClientError> {
		Ok(match maybe_block_number {
			Some(number) => self.client.request("chain_getBlockHash", rpc_params![number]).await?,
			None => self.client.request("chain_getBlockHash", rpc_params![]).await?,
		})
	}

	pub async fn legacy_get_chain_name(&self) -> Result<String, ClientError> {
		self.client.request("system_chain", rpc_params![]).await
	}

	pub async fn stream_best_block_headers(&self) -> Result<HeaderStream, ClientError> {
		self.subscribe_headers("chain_subscribeNewHeads", "chain_unsubscribeNewHeads")
			.await
	}

	/// Finalized headers, with blocks skipped by finality jumps filled in.
	pub async fn stream_finalized_block_headers(&self) -> Result<HeaderStream, ClientError> {
		let headers = self
			.subscribe_headers("chain_subscribeFinalizedHeads", "chain_unsubscribeFinalizedHeads")
			.await?;
		let client = self.clone();
		let last_number = Arc::new(Mutex::new(None::<BlockNumber>));
		Ok(HeaderStream(
			headers
				.then(move |item| {
					let client = client.clone();
					let last_number = last_number.clone();
					async move {
						let Ok((header, _)) = &item else { return vec![item] };
						let from = last_number.lock().unwrap().map_or(header.number, |n| n + 1);
						// Walk back via `parent_hash`; on failure the next header retries the gap.
						let mut gap = Vec::new();
						let (mut hash, mut number) = (header.parent_hash, header.number);
						while number > from {
							number -= 1;
							match client.header_via_rpc(hash).await {
								Ok(parent) => {
									gap.push(Ok((parent.clone(), hash)));
									hash = parent.parent_hash;
								},
								Err(e) => return vec![Err(e)],
							}
						}
						let mut items: Vec<_> = gap.into_iter().rev().collect();
						*last_number.lock().unwrap() = Some(header.number);
						items.push(item);
						items
					}
				})
				.flat_map(stream::iter)
				.boxed(),
		))
	}

	async fn subscribe_headers(&self, method: &str, unsubscribe: &str) -> Result<HeaderStream, ClientError> {
		let subscription = self.client.subscribe::<Header>(method, unsubscribe, rpc_params![]).await?;
		Ok(HeaderStream(
			subscription
				.map(|item| {
					let header = item?;
					let hash = header_hash(&header);
					Ok((header, hash))
				})
				.boxed(),
		))
	}
}

/// How long to wait for each finalized head before starting the light client.
const LIGHT_BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(60);

/// Builds a client for `url`. In light mode the chain spec is fetched from the node at `url`
/// and the chain is then followed by an embedded smoldot instead.
pub async fn build_online_client(url: &str, mode: ApiClientMode) -> Result<ApiClient, String> {
	let ws = WsClientBuilder::default()
		.max_buffer_capacity_per_subscription(4096)
		.build(url)
		.await
		.map_err(|e| format!("Cannot construct RPC client: {e}"))?;
	let client = match mode {
		ApiClientMode::RPC => Transport::Ws(Arc::new(ws)),
		ApiClientMode::Light => {
			let chainspec: Box<serde_json::value::RawValue> = ws
				.request("sync_state_genSyncSpec", rpc_params![true])
				.await
				.map_err(|e| format!("Cannot fetch chainspec: {e}"))?;
			// Skip two finalized heads so smoldot doesn't start from the chain spec's block.
			let mut heads = ws
				.subscribe::<Box<serde_json::value::RawValue>, _>(
					"chain_subscribeFinalizedHeads",
					rpc_params![],
					"chain_unsubscribeFinalizedHeads",
				)
				.await
				.map_err(|e| format!("Cannot subscribe to finalized heads: {e}"))?;
			for _ in 0..2 {
				match tokio::time::timeout(LIGHT_BOOTSTRAP_TIMEOUT, heads.next()).await {
					Ok(Some(Ok(_))) => {},
					Ok(Some(Err(e))) => return Err(format!("Finalized heads subscription failed: {e}")),
					Ok(None) => return Err("Finalized heads subscription closed during light client bootstrap".into()),
					Err(_) => return Err("Timed out waiting for finalized heads during light client bootstrap".into()),
				}
			}
			let (_client, rpc) =
				LightClient::relay_chain(chainspec.get()).map_err(|e| format!("Cannot construct LightClient: {e}"))?;
			Transport::Light(rpc)
		},
	};
	Ok(ApiClient { client })
}

async fn join_requests<I, T>(fut: I) -> Result<Vec<T>, ClientError>
where
	I: IntoIterator<Item = tokio::task::JoinHandle<Result<T, ClientError>>>,
{
	futures::future::try_join_all(fut)
		.await
		.map_err(|e| ClientError::Other(format!("Cannot join requests: {:?}", e)))?
		.into_iter()
		.collect::<Result<Vec<_>, ClientError>>()
}
