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
	api::{
		decode::{
			DecodedBabeEpoch, DecodedDispute, babe_current_slot_key, decode_account_keys, decode_availability_cores,
			decode_babe_epoch, decode_candidate_events, decode_claim_queue, decode_disputes, decode_hrmp_channel,
			decode_hrmp_channel_digests, decode_para_ids, decode_session_index, decode_slot, decode_timestamp,
			decode_validator_groups, hrmp_channel_digests_key, hrmp_channels_key, hrmp_egress_channels_index_key,
			para_session_account_keys_key, timestamp_now_key,
		},
		dynamic::{
			decode_availability_cores as decode_availability_cores_dynamic, decode_inherent_data,
			decode_validator_groups as decode_validator_groups_dynamic,
		},
		shadow,
	},
	chain_events::SubxtCandidateEvent,
	metadata::{
		polkadot::{
			self,
			runtime_types::{
				polkadot_parachain_primitives::primitives::{HrmpChannelId, Id},
				polkadot_runtime_parachains::hrmp::HrmpChannel,
				sp_consensus_babe::{self, digests::PreDigest},
				sp_consensus_slots::Slot,
				sp_core::crypto::KeyTypeId,
				sp_runtime::generic::digest::DigestItem,
			},
		},
		polkadot_primitives::ValidatorIndex,
	},
	types::{
		AccountId32, BlockNumber, ClaimQueue, CoreOccupied, H256, Header, InherentData, PolkadotHasher, QueuedKeys,
		SessionKeys, SubxtHrmpChannel, Timestamp,
	},
};
use clap::ValueEnum;
use parity_scale_codec::Decode;
use std::collections::BTreeMap;
use subxt::{
	OnlineClient, PolkadotConfig,
	backend::{
		StreamOf,
		legacy::{LegacyRpcMethods, rpc_methods::NumberOrHex},
		rpc::RpcClient,
	},
	blocks::{Block, BlockRef, BlocksClient},
	client::OnlineClientT,
	dynamic::Value,
	events::{Events, EventsClient},
	lightclient::LightClient,
	runtime_api::{RuntimeApi, RuntimeApiClient},
	storage::StorageClient,
	utils::fetch_chainspec_from_rpc_node,
};

pub type HeaderStream = StreamOf<Result<(Header, BlockRef<H256>), subxt::Error>>;

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
	hasher: PolkadotHasher,
	/// When set, each migrated read is also decoded through the metadata-free path and the two
	/// results are compared, aborting on any mismatch. The comparison harness itself lands with
	/// the first migrated read.
	shadow: bool,
}

impl<T: OnlineClientT<PolkadotConfig>> ApiClient<T> {
	pub fn hasher(&self) -> PolkadotHasher {
		self.hasher
	}

	/// Whether the shadow-decode harness is enabled for this client.
	pub fn shadow_enabled(&self) -> bool {
		self.shadow
	}

	fn storage(&self) -> StorageClient<PolkadotConfig, T> {
		self.client.storage()
	}

	fn blocks(&self) -> BlocksClient<PolkadotConfig, T> {
		self.client.blocks()
	}

	fn events(&self) -> EventsClient<PolkadotConfig, T> {
		self.client.events()
	}

	fn runtime_api(&self) -> RuntimeApiClient<PolkadotConfig, T> {
		self.client.runtime_api()
	}

	async fn block_at(&self, maybe_hash: Option<H256>) -> Result<Block<PolkadotConfig, T>, subxt::Error> {
		match maybe_hash {
			Some(hash) => self.blocks().at(hash).await,
			None => self.blocks().at_latest().await,
		}
	}

	async fn runtime_api_at(&self, maybe_hash: Option<H256>) -> Result<RuntimeApi<PolkadotConfig, T>, subxt::Error> {
		match maybe_hash {
			Some(hash) => Ok(self.runtime_api().at(hash)),
			None => self.runtime_api().at_latest().await,
		}
	}

	async fn get_hrmp_egress_channels_index(
		storage: StorageClient<PolkadotConfig, T>,
		legacy_rpc_methods: LegacyRpcMethods<PolkadotConfig>,
		shadow: bool,
		block_hash: H256,
		para_id: u32,
	) -> Result<Vec<u32>, subxt::Error> {
		let addr = polkadot::storage().hrmp().hrmp_egress_channels_index(Id(para_id));
		let index: Vec<u32> = storage
			.at(block_hash)
			.fetch(&addr)
			.await?
			.unwrap_or_default()
			.iter()
			.map(|id| id.0)
			.collect();

		if shadow {
			let metadata_free = legacy_rpc_methods
				.state_get_storage(&hrmp_egress_channels_index_key(para_id), Some(block_hash))
				.await?
				.map(|bytes| decode_para_ids(&bytes))
				.transpose()
				.map_err(|e| {
					subxt::Error::Other(format!("Failed to decode HrmpEgressChannelsIndex (metadata-free): {e}"))
				})?
				.unwrap_or_default();
			shadow::compare(block_hash, "hrmp_egress_channels_index", &index, &metadata_free);
		}

		Ok(index)
	}

	async fn get_hrmp_channel_digests(
		storage: StorageClient<PolkadotConfig, T>,
		legacy_rpc_methods: LegacyRpcMethods<PolkadotConfig>,
		shadow: bool,
		block_hash: H256,
		para_id: u32,
	) -> Result<Vec<(u32, Vec<u32>)>, subxt::Error> {
		let addr = polkadot::storage().hrmp().hrmp_channel_digests(Id(para_id));
		let digests: Vec<(u32, Vec<u32>)> = storage
			.at(block_hash)
			.fetch(&addr)
			.await?
			.unwrap_or_default()
			.into_iter()
			.map(|v| (v.0, v.1.into_iter().map(|v| v.0).collect()))
			.collect();

		if shadow {
			let metadata_free = legacy_rpc_methods
				.state_get_storage(&hrmp_channel_digests_key(para_id), Some(block_hash))
				.await?
				.map(|bytes| decode_hrmp_channel_digests(&bytes))
				.transpose()
				.map_err(|e| subxt::Error::Other(format!("Failed to decode HrmpChannelDigests (metadata-free): {e}")))?
				.unwrap_or_default();
			shadow::compare(block_hash, "hrmp_channel_digests", &digests, &metadata_free);
		}

		Ok(digests)
	}

	async fn get_hrmp_channels(
		storage: StorageClient<PolkadotConfig, T>,
		legacy_rpc_methods: LegacyRpcMethods<PolkadotConfig>,
		shadow: bool,
		block_hash: H256,
		sender: u32,
		recipient: u32,
	) -> Result<Option<(u32, u32, HrmpChannel)>, subxt::Error> {
		let id = HrmpChannelId { sender: Id(sender), recipient: Id(recipient) };
		let addr = polkadot::storage().hrmp().hrmp_channels(id);
		let channel = storage.at(block_hash).fetch(&addr).await?.map(|v| (sender, recipient, v));

		if shadow {
			let metadata_free: Option<SubxtHrmpChannel> = legacy_rpc_methods
				.state_get_storage(&hrmp_channels_key(sender, recipient), Some(block_hash))
				.await?
				.map(|bytes| decode_hrmp_channel(&bytes))
				.transpose()
				.map_err(|e| subxt::Error::Other(format!("Failed to decode HrmpChannels (metadata-free): {e}")))?;
			let old = channel.as_ref().map(|(_, _, channel)| SubxtHrmpChannel::from(channel));
			shadow::compare(block_hash, "hrmp_channel", &old, &metadata_free);
		}

		Ok(channel)
	}

	async fn get_inbound_hrmp_channel_pairs(
		&self,
		block_hash: H256,
		para_ids: Vec<u32>,
	) -> color_eyre::Result<Vec<(u32, u32)>, subxt::Error> {
		let inbound_ids_fut = para_ids.iter().map(|&para_id| {
			tokio::spawn(Self::get_hrmp_channel_digests(
				self.storage(),
				self.legacy_rpc_methods.clone(),
				self.shadow,
				block_hash,
				para_id,
			))
		});
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
	) -> color_eyre::Result<Vec<(u32, u32)>, subxt::Error> {
		let outbound_ids_fut = para_ids.iter().map(|&para_id| {
			tokio::spawn(Self::get_hrmp_egress_channels_index(
				self.storage(),
				self.legacy_rpc_methods.clone(),
				self.shadow,
				block_hash,
				para_id,
			))
		});
		let outbound_ids: Vec<Vec<u32>> = join_requests(outbound_ids_fut).await?;

		Ok(para_ids
			.into_iter()
			.zip(outbound_ids)
			.flat_map(|(para_id, ids)| -> Vec<_> { ids.into_iter().map(|recipient| (para_id, recipient)).collect() })
			.collect())
	}
}

impl<T: OnlineClientT<PolkadotConfig>> ApiClient<T> {
	pub async fn get_head(&self, maybe_hash: Option<H256>) -> Result<Header, subxt::Error> {
		Ok(self.block_at(maybe_hash).await?.header().clone())
	}

	pub async fn get_block_number(&self, maybe_hash: Option<H256>) -> Result<BlockNumber, subxt::Error> {
		Ok(self.block_at(maybe_hash).await?.number())
	}

	pub async fn get_block_ts(&self, hash: H256) -> Result<Option<Timestamp>, subxt::Error> {
		let addr = polkadot::storage().timestamp().now();
		let timestamp = self.storage().at(hash).fetch(&addr).await?;

		if self.shadow {
			let metadata_free = self
				.legacy_rpc_methods
				.state_get_storage(&timestamp_now_key(), Some(hash))
				.await?
				.map(|bytes| decode_timestamp(&bytes))
				.transpose()
				.map_err(|e| subxt::Error::Other(format!("Failed to decode Timestamp.Now (metadata-free): {e}")))?;
			shadow::compare(hash, "block_timestamp", &timestamp, &metadata_free);
		}

		Ok(timestamp)
	}

	pub async fn get_events(&self, hash: H256) -> Result<Events<PolkadotConfig>, subxt::Error> {
		self.events().at(hash).await
	}

	/// Reads candidate (backed/included/timed-out) events for a block through the
	/// `ParachainHost_candidate_events` runtime call and decodes them without metadata.
	pub async fn get_candidate_events(&self, hash: H256) -> Result<Vec<SubxtCandidateEvent>, subxt::Error> {
		let bytes = self
			.legacy_rpc_methods
			.state_call("ParachainHost_candidate_events", None, Some(hash))
			.await?;
		decode_candidate_events(&bytes, self.hasher)
			.map_err(|e| subxt::Error::Other(format!("Failed to decode candidate_events: {e}")))
	}

	/// Reads the recent disputes for a block through the `ParachainHost_disputes` runtime call and
	/// decodes them without metadata.
	pub async fn get_disputes(&self, hash: H256) -> Result<Vec<DecodedDispute>, subxt::Error> {
		let bytes = self
			.legacy_rpc_methods
			.state_call("ParachainHost_disputes", None, Some(hash))
			.await?;
		decode_disputes(&bytes).map_err(|e| subxt::Error::Other(format!("Failed to decode disputes: {e}")))
	}

	/// Reads the current Babe epoch through the `BabeApi_current_epoch` runtime call, decoded without
	/// metadata. Used to shadow the `Babe.Randomness` and `Babe.Authorities` storage reads.
	async fn babe_current_epoch(&self, hash: H256) -> Result<DecodedBabeEpoch, subxt::Error> {
		let bytes = self
			.legacy_rpc_methods
			.state_call("BabeApi_current_epoch", None, Some(hash))
			.await?;
		decode_babe_epoch(&bytes)
			.map_err(|e| subxt::Error::Other(format!("Failed to decode BabeApi_current_epoch: {e}")))
	}

	pub async fn get_babe_randomness(&self, hash: H256) -> Result<Option<[u8; 32]>, subxt::Error> {
		let addr = polkadot::storage().babe().randomness();
		let randomness = self.storage().at(hash).fetch(&addr).await?;

		if self.shadow {
			let epoch = self.babe_current_epoch(hash).await?;
			shadow::compare(hash, "babe_randomness", &randomness, &Some(epoch.randomness));
		}

		Ok(randomness)
	}

	pub async fn get_babe_authorities(
		&self,
		hash: H256,
	) -> Result<Vec<(sp_consensus_babe::app::Public, u64)>, subxt::Error> {
		let addr = polkadot::storage().babe().authorities();
		let authorities = self
			.storage()
			.at(hash)
			.fetch(&addr)
			.await
			.map(|res| res.map(|res| res.0).unwrap_or_default())?;

		if self.shadow {
			let epoch = self.babe_current_epoch(hash).await?;
			// `app::Public` wraps the 32 raw key bytes, matching the metadata-free `[u8; 32]`.
			let old: Vec<([u8; 32], u64)> = authorities.iter().map(|(public, weight)| (public.0, *weight)).collect();
			shadow::compare(hash, "babe_authorities", &old, &epoch.authorities);
		}

		Ok(authorities)
	}

	pub async fn get_babe_current_slot(&self, hash: H256) -> Result<Option<Slot>, subxt::Error> {
		let addr = polkadot::storage().babe().current_slot();
		let slot = self.storage().at(hash).fetch(&addr).await?;

		if self.shadow {
			let metadata_free = self
				.legacy_rpc_methods
				.state_get_storage(&babe_current_slot_key(), Some(hash))
				.await?
				.map(|bytes| decode_slot(&bytes))
				.transpose()
				.map_err(|e| subxt::Error::Other(format!("Failed to decode Babe.CurrentSlot (metadata-free): {e}")))?;
			shadow::compare(hash, "babe_current_slot", &slot.as_ref().map(|slot| slot.0), &metadata_free);
		}

		Ok(slot)
	}

	pub async fn get_system_digest(&self, hash: H256) -> Result<Option<PreDigest>, subxt::Error> {
		let addr = polkadot::storage().system().digest();
		let result = self.storage().at(hash).fetch(&addr).await.unwrap().unwrap();

		for pre_digest in result.logs.into_iter() {
			if let DigestItem::PreRuntime(_, bytes) = pre_digest {
				let pre_digest: PreDigest = PreDigest::decode(&mut &bytes[..]).unwrap();
				return Ok(Some(pre_digest));
			}
		}

		Ok(None)
	}

	pub async fn get_babe_key_owner(&self, hash: H256, public: &[u8]) -> Result<Option<AccountId32>, subxt::Error> {
		let addr = polkadot::storage().session().key_owner((KeyTypeId(*b"babe"), public.to_vec()));
		self.storage().at(hash).fetch(&addr).await
	}

	pub async fn get_occupied_cores(&self, hash: H256) -> Result<Vec<CoreOccupied>, subxt::Error> {
		let addr = subxt::runtime_api::dynamic("ParachainHost", "availability_cores", Vec::<Value<()>>::new());
		let value = self.runtime_api_at(Some(hash)).await?.call(addr).await?.to_value()?;
		let cores = decode_availability_cores_dynamic(&value)
			.map_err(|e| subxt::Error::Other(format!("Failed to decode availability_cores: {e}")))?;

		if self.shadow {
			let bytes = self
				.legacy_rpc_methods
				.state_call("ParachainHost_availability_cores", None, Some(hash))
				.await?;
			let metadata_free = decode_availability_cores(&bytes).map_err(|e| {
				subxt::Error::Other(format!("Failed to decode availability_cores (metadata-free): {e}"))
			})?;
			shadow::compare(hash, "availability_cores", &cores, &metadata_free);
		}

		Ok(cores)
	}

	pub async fn get_claim_queue(&self, hash: H256) -> Result<ClaimQueue, subxt::Error> {
		let addr = polkadot::apis().parachain_host().claim_queue();
		let queue: ClaimQueue = self.runtime_api_at(Some(hash)).await?.call(addr).await.map(|queue| {
			queue
				.iter()
				.map(|(core, ids)| {
					let core = core.0;
					let ids = ids.iter().map(|id| id.0).collect::<Vec<_>>();
					(core, ids)
				})
				.collect::<Vec<_>>()
		})?;

		if self.shadow {
			let bytes = self
				.legacy_rpc_methods
				.state_call("ParachainHost_claim_queue", None, Some(hash))
				.await?;
			let metadata_free = decode_claim_queue(&bytes)
				.map_err(|e| subxt::Error::Other(format!("Failed to decode claim_queue (metadata-free): {e}")))?;
			shadow::compare(hash, "claim_queue", &queue, &metadata_free);
		}

		Ok(queue)
	}

	pub async fn get_backing_groups(&self, hash: H256) -> Result<Vec<Vec<ValidatorIndex>>, subxt::Error> {
		let value = self
			.fetch_dynamic_storage(Some(hash), "ParaScheduler", "ValidatorGroups")
			.await?
			.ok_or_else(|| subxt::Error::Other("ParaScheduler.ValidatorGroups not found".to_string()))?;
		let groups = decode_validator_groups_dynamic(&value)
			.map_err(|e| subxt::Error::Other(format!("Failed to decode validator groups: {e}")))?;

		if self.shadow {
			let bytes = self
				.legacy_rpc_methods
				.state_call("ParachainHost_validator_groups", None, Some(hash))
				.await?;
			let metadata_free = decode_validator_groups(&bytes)
				.map_err(|e| subxt::Error::Other(format!("Failed to decode validator_groups (metadata-free): {e}")))?;
			let old: Vec<Vec<u32>> = groups.iter().map(|group| group.iter().map(|idx| idx.0).collect()).collect();
			shadow::compare(hash, "validator_groups", &old, &metadata_free);
		}

		Ok(groups)
	}

	pub async fn get_session_index(&self, hash: H256) -> Result<Option<u32>, subxt::Error> {
		let addr = polkadot::storage().session().current_index();
		let index = self.storage().at(hash).fetch(&addr).await?;

		if self.shadow {
			let bytes = self
				.legacy_rpc_methods
				.state_call("ParachainHost_session_index_for_child", None, Some(hash))
				.await?;
			let metadata_free = decode_session_index(&bytes)
				.map_err(|e| subxt::Error::Other(format!("Failed to decode session_index_for_child: {e}")))?;
			shadow::compare(hash, "session_index", &index.unwrap_or_default(), &metadata_free);
		}

		Ok(index)
	}

	pub async fn get_session_index_now(&self) -> Result<Option<u32>, subxt::Error> {
		let addr = polkadot::storage().session().current_index();
		self.storage().at_latest().await?.fetch(&addr).await
	}

	pub async fn get_session_account_keys(
		&self,
		session_index: u32,
		maybe_hash: Option<H256>,
	) -> Result<Option<Vec<AccountId32>>, subxt::Error> {
		let addr = polkadot::storage().para_session_info().account_keys(session_index);

		// Query session keys at a specific block hash.
		let storage =
			if let Some(hash) = maybe_hash { self.storage().at(hash) } else { self.storage().at_latest().await? };

		let keys = storage.fetch(&addr).await?;

		// Shadow only at a concrete hash; at latest the typed and raw reads could race to different blocks.
		if self.shadow &&
			let Some(hash) = maybe_hash
		{
			let key = para_session_account_keys_key(session_index);
			let metadata_free = self
				.legacy_rpc_methods
				.state_get_storage(&key, Some(hash))
				.await?
				.map(|bytes| decode_account_keys(&bytes))
				.transpose()
				.map_err(|e| subxt::Error::Other(format!("Failed to decode account_keys (metadata-free): {e}")))?;
			shadow::compare(hash, "session_account_keys", &keys, &metadata_free);
		}

		Ok(keys)
	}

	pub async fn get_session_next_keys(&self, account: &AccountId32) -> Result<Option<SessionKeys>, subxt::Error> {
		let addr = polkadot::storage().session().next_keys(account.clone());
		self.storage().at_latest().await?.fetch(&addr).await
	}

	pub async fn get_session_queued_keys(&self, hash: Option<H256>) -> Result<Option<QueuedKeys>, subxt::Error> {
		let addr = polkadot::storage().session().queued_keys();
		if let Some(hash) = hash {
			self.storage().at(hash).fetch(&addr).await
		} else {
			self.storage().at_latest().await?.fetch(&addr).await
		}
	}

	pub async fn get_inbound_outbound_hrmp_channels(
		&self,
		block_hash: H256,
		para_ids: Vec<u32>,
	) -> color_eyre::Result<Vec<(u32, BTreeMap<u32, SubxtHrmpChannel>, BTreeMap<u32, SubxtHrmpChannel>)>, subxt::Error>
	{
		let inbound_pairs = self.get_inbound_hrmp_channel_pairs(block_hash, para_ids.clone()).await?;
		let inbound_channels_fut = inbound_pairs.iter().map(|(sender, para_id)| {
			tokio::spawn(Self::get_hrmp_channels(
				self.storage(),
				self.legacy_rpc_methods.clone(),
				self.shadow,
				block_hash,
				*sender,
				*para_id,
			))
		});
		let inbound_channels: Vec<_> = join_requests(inbound_channels_fut).await?.into_iter().flatten().collect();

		let mut inbound_by_para_id: BTreeMap<u32, BTreeMap<u32, SubxtHrmpChannel>> = BTreeMap::new();
		for (sender, para_id, channel) in inbound_channels {
			let channels = inbound_by_para_id.entry(para_id).or_default();
			channels.insert(sender, channel.into());
		}

		let outbound_pairs = self.get_outbound_hrmp_channel_pairs(block_hash, para_ids.clone()).await?;
		let outbound_channels_fut = outbound_pairs.iter().map(|(para_id, recipient)| {
			tokio::spawn(Self::get_hrmp_channels(
				self.storage(),
				self.legacy_rpc_methods.clone(),
				self.shadow,
				block_hash,
				*para_id,
				*recipient,
			))
		});
		let outbound_channels: Vec<_> = join_requests(outbound_channels_fut).await?.into_iter().flatten().collect();

		let mut outbound_by_para_id: BTreeMap<u32, BTreeMap<u32, SubxtHrmpChannel>> = BTreeMap::new();
		for (para_id, recipient, channel) in outbound_channels {
			let channels = outbound_by_para_id.entry(para_id).or_default();
			channels.insert(recipient, channel.into());
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

	pub async fn fetch_dynamic_storage(
		&self,
		maybe_hash: Option<H256>,
		pallet_name: &str,
		entry_name: &str,
	) -> Result<Option<Value<u32>>, subxt::Error> {
		let storage = match maybe_hash {
			Some(hash) => self.storage().at(hash),
			None => self.storage().at_latest().await?,
		};
		match storage.fetch(&subxt::dynamic::storage(pallet_name, entry_name, vec![])).await? {
			Some(v) => Ok(Some(v.to_value()?)),
			None => Ok(None),
		}
	}

	pub async fn extract_parainherent(&self, maybe_hash: Option<H256>) -> Result<InherentData, subxt::Error> {
		let block = self.block_at(maybe_hash).await?;
		let ex = block
			.extrinsics()
			.await?
			.iter()
			.take(2)
			.last()
			.ok_or_else(|| "`ParaInherent` data is always at index #1".to_string())?;
		ex.field_values()
			.map_err(|e| format!("Failed to get ParaInherent field values: {e}"))
			.and_then(|v| decode_inherent_data(&v).map_err(|e| format!("Failed to decode ParaInherent: {e}")))
			.map_err(subxt::Error::Other)
	}

	// We need it only for the historical mode to convert block numbers into their hashes
	pub async fn legacy_get_block_hash(
		&self,
		maybe_block_number: Option<BlockNumber>,
	) -> Result<Option<H256>, subxt::Error> {
		let maybe_block_number = maybe_block_number.map(|v| NumberOrHex::Number(v.into()));
		Ok(self.legacy_rpc_methods.chain_get_block_hash(maybe_block_number).await?)
	}

	pub async fn legacy_get_chain_name(&self) -> Result<String, subxt::Error> {
		Ok(self.legacy_rpc_methods.system_chain().await?)
	}

	pub async fn stream_best_block_headers(&self) -> Result<HeaderStream, subxt::Error> {
		self.client.backend().stream_best_block_headers(self.hasher()).await
	}

	pub async fn stream_finalized_block_headers(&self) -> Result<HeaderStream, subxt::Error> {
		self.client.backend().stream_finalized_block_headers(self.hasher()).await
	}
}

pub async fn build_online_client(
	url: &str,
	mode: ApiClientMode,
	shadow: bool,
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

	Ok(ApiClient { client, legacy_rpc_methods, hasher, shadow })
}

async fn join_requests<I, T>(fut: I) -> Result<Vec<T>, subxt::Error>
where
	I: IntoIterator<Item = tokio::task::JoinHandle<Result<T, subxt::Error>>>,
{
	futures::future::try_join_all(fut)
		.await
		.map_err(|e| subxt::Error::Other(format!("Cannot join requests: {:?}", e)))?
		.into_iter()
		.collect::<Result<Vec<_>, subxt::Error>>()
}
