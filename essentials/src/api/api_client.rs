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

// `subxt::Error` is large; this goes away together with subxt.
#![allow(clippy::result_large_err)]

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
		SubxtHrmpChannel, Timestamp, ValidatorIndex,
	},
};
use clap::ValueEnum;
use futures::StreamExt;
use parity_scale_codec::{Decode, Encode};
use std::collections::BTreeMap;
use subxt::{
	OnlineClient, PolkadotConfig,
	backend::{
		StreamOf,
		legacy::{LegacyRpcMethods, rpc_methods::NumberOrHex},
		rpc::RpcClient,
	},
	blocks::BlockRef,
	client::OnlineClientT,
	lightclient::LightClient,
	utils::fetch_chainspec_from_rpc_node,
};
use thiserror::Error;

/// A live stream of `(header, block hash)` pairs from a chain-head subscription.
pub type HeaderStream = StreamOf<Result<(Header, H256), ClientError>>;

/// Error from the RPC client: transport failures, undecodable responses, or missing data.
#[derive(Debug, Error)]
pub enum ClientError {
	#[error("RPC error: {0}")]
	Rpc(#[from] subxt::Error),
	#[error("{0}")]
	Other(String),
}

impl From<subxt::ext::subxt_rpcs::Error> for ClientError {
	fn from(err: subxt::ext::subxt_rpcs::Error) -> Self {
		Self::Rpc(err.into())
	}
}

/// Converts a transport header into the owned one through its SCALE encoding.
fn into_header(header: impl Encode) -> Result<Header, ClientError> {
	Header::decode(&mut &header.encode()[..]).map_err(|e| ClientError::Other(format!("Cannot decode header: {e}")))
}

/// How to subscribe to subxt blocks
#[derive(strum::Display, Debug, Clone, Copy, ValueEnum, Default)]
pub enum ApiClientMode {
	#[default]
	RPC,
	Light,
}

#[derive(Clone)]
pub struct ApiClient<T>
where
	T: OnlineClientT<PolkadotConfig>,
{
	client: T,
	legacy_rpc_methods: LegacyRpcMethods<PolkadotConfig>,
	hasher: <PolkadotConfig as subxt::Config>::Hasher,
}

impl<T: OnlineClientT<PolkadotConfig>> ApiClient<T> {
	/// Fetches a header via `chain_getHeader`, returning `None` when the node has no header for the
	/// hash (e.g. a pruned block in historical mode).
	async fn maybe_header_via_rpc(&self, hash: H256) -> Result<Option<Header>, ClientError> {
		self.legacy_rpc_methods
			.chain_get_header(Some(hash))
			.await?
			.map(into_header)
			.transpose()
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
			None => Ok(self.legacy_rpc_methods.chain_get_finalized_head().await?),
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
		self.legacy_rpc_methods
			.state_get_storage(key, Some(hash))
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
		let bytes = self.legacy_rpc_methods.state_call(method, None, Some(hash)).await?;
		decode(&bytes).map_err(|e| ClientError::Other(format!("Failed to decode {method}: {e}")))
	}

	async fn get_hrmp_egress_channels_index(
		client: ApiClient<T>,
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
		client: ApiClient<T>,
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
		client: ApiClient<T>,
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

impl<T: OnlineClientT<PolkadotConfig>> ApiClient<T> {
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
		let details = self
			.legacy_rpc_methods
			.chain_get_block(Some(hash))
			.await?
			.ok_or_else(|| ClientError::Other(format!("block {hash:?} not found via chain_getBlock")))?;
		let para_inherent = details
			.block
			.extrinsics
			.get(1)
			.ok_or_else(|| ClientError::Other("`ParaInherent` extrinsic missing at index #1".to_string()))?;
		decode_parainherent(&para_inherent.0)
			.map_err(|e| ClientError::Other(format!("Failed to decode ParaInherent: {e}")))
	}

	// We need it only for the historical mode to convert block numbers into their hashes
	pub async fn legacy_get_block_hash(
		&self,
		maybe_block_number: Option<BlockNumber>,
	) -> Result<Option<H256>, ClientError> {
		let maybe_block_number = maybe_block_number.map(|v| NumberOrHex::Number(v.into()));
		Ok(self.legacy_rpc_methods.chain_get_block_hash(maybe_block_number).await?)
	}

	pub async fn legacy_get_chain_name(&self) -> Result<String, ClientError> {
		Ok(self.legacy_rpc_methods.system_chain().await?)
	}

	pub async fn stream_best_block_headers(&self) -> Result<HeaderStream, ClientError> {
		Ok(Self::header_stream(self.client.backend().stream_best_block_headers(self.hasher).await?))
	}

	pub async fn stream_finalized_block_headers(&self) -> Result<HeaderStream, ClientError> {
		Ok(Self::header_stream(self.client.backend().stream_finalized_block_headers(self.hasher).await?))
	}

	fn header_stream<H: Encode + 'static>(stream: StreamOf<Result<(H, BlockRef<H256>), subxt::Error>>) -> HeaderStream {
		StreamOf::new(Box::pin(stream.map(|item| {
			let (header, block_ref) = item?;
			Ok((into_header(header)?, block_ref.hash()))
		})))
	}
}

pub async fn build_online_client(
	url: &str,
	mode: ApiClientMode,
) -> Result<ApiClient<OnlineClient<PolkadotConfig>>, String> {
	let (client, rpc_client) = match mode {
		ApiClientMode::RPC => {
			let rpc_client = RpcClient::from_url(url)
				.await
				.map_err(|e| format!("Cannot construct RPC client: {e}"))?;
			let client = OnlineClient::from_rpc_client(rpc_client.clone())
				.await
				.map_err(|e| format!("Cannot construct OnlineClient from rpc client: {e}"))?;
			(client, rpc_client)
		},
		ApiClientMode::Light => {
			let chainspec = fetch_chainspec_from_rpc_node(url)
				.await
				.map_err(|e| format!("Cannot fetch chainspec: {e}"))?;
			let (_client, rpc_client) =
				LightClient::relay_chain(chainspec.get()).map_err(|e| format!("Cannot construct LightClient: {e}"))?;
			let client = OnlineClient::from_rpc_client(rpc_client.clone())
				.await
				.map_err(|e| format!("Cannot construct OnlineClient from rpc client: {e}"))?;
			(client, rpc_client.into())
		},
	};
	let legacy_rpc_methods = LegacyRpcMethods::<PolkadotConfig>::new(rpc_client);
	let hasher = client.hasher();

	Ok(ApiClient { client, legacy_rpc_methods, hasher })
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
