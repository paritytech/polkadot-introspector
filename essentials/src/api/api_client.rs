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
	metadata::{
		polkadot::{
			self,
			runtime_types::{
				polkadot_parachain_primitives::primitives::{HrmpChannelId, Id},
				polkadot_runtime_parachains::hrmp::HrmpChannel,
			},
		},
		polkadot_staging_primitives::CoreState,
	},
	types::{
		AccountId32, BlockNumber, ClaimQueue, Header, InherentData, QueuedKeys, SessionKeys, SubxtHrmpChannel,
		Timestamp, H256,
	},
};
use clap::ValueEnum;
use std::collections::BTreeMap;
use subxt::{
	backend::{
		legacy::{rpc_methods::NumberOrHex, LegacyRpcMethods},
		rpc::RpcClient,
		StreamOf,
	},
	blocks::{Block, BlockRef, BlocksClient},
	client::OnlineClientT,
	dynamic::Value,
	events::{Events, EventsClient},
	lightclient::LightClient,
	runtime_api::{RuntimeApi, RuntimeApiClient},
	storage::StorageClient,
	utils::fetch_chainspec_from_rpc_node,
	OnlineClient, PolkadotConfig,
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
}

impl<T: OnlineClientT<PolkadotConfig>> ApiClient<T> {
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
		block_hash: H256,
		para_id: u32,
	) -> Result<Vec<u32>, subxt::Error> {
		let addr = polkadot::storage().hrmp().hrmp_egress_channels_index(Id(para_id));
		Ok(storage
			.at(block_hash)
			.fetch(&addr)
			.await?
			.unwrap_or_default()
			.iter()
			.map(|id| id.0)
			.collect())
	}

	async fn get_hrmp_channel_digests(
		storage: StorageClient<PolkadotConfig, T>,
		block_hash: H256,
		para_id: u32,
	) -> Result<Vec<(u32, Vec<u32>)>, subxt::Error> {
		let addr = polkadot::storage().hrmp().hrmp_channel_digests(Id(para_id));
		Ok(storage
			.at(block_hash)
			.fetch(&addr)
			.await?
			.unwrap_or_default()
			.into_iter()
			.map(|v| (v.0, v.1.into_iter().map(|v| v.0).collect()))
			.collect())
	}

	async fn get_hrmp_channels(
		storage: StorageClient<PolkadotConfig, T>,
		block_hash: H256,
		sender: u32,
		recipient: u32,
	) -> Result<Option<(u32, u32, HrmpChannel)>, subxt::Error> {
		let id = HrmpChannelId { sender: Id(sender), recipient: Id(recipient) };
		let addr = polkadot::storage().hrmp().hrmp_channels(&id);
		Ok(storage.at(block_hash).fetch(&addr).await?.map(|v| (sender, recipient, v)))
	}

	async fn get_inbound_hrmp_channel_pairs(
		&self,
		block_hash: H256,
		para_ids: Vec<u32>,
	) -> color_eyre::Result<Vec<(u32, u32)>, subxt::Error> {
		let inbound_ids_fut = para_ids
			.iter()
			.map(|&para_id| tokio::spawn(Self::get_hrmp_channel_digests(self.storage(), block_hash, para_id)));
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
		let outbound_ids_fut = para_ids
			.iter()
			.map(|&para_id| tokio::spawn(Self::get_hrmp_egress_channels_index(self.storage(), block_hash, para_id)));
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
		let timestamp = polkadot::storage().timestamp().now();
		self.storage().at(hash).fetch(&timestamp).await
	}

	pub async fn get_events(&self, hash: H256) -> Result<Events<PolkadotConfig>, subxt::Error> {
		self.events().at(hash).await
	}

	pub async fn get_occupied_cores(&self, hash: H256) -> Result<Vec<CoreState<H256, u32>>, subxt::Error> {
		let addr = polkadot::apis().parachain_host().availability_cores();
		self.runtime_api_at(Some(hash)).await?.call(addr).await
	}

	pub async fn get_claim_queue(&self, hash: H256) -> Result<ClaimQueue, subxt::Error> {
		let addr = polkadot::apis().parachain_host().claim_queue();
		self.runtime_api_at(Some(hash)).await?.call(addr).await.map(|queue| {
			queue
				.iter()
				.map(|(core, ids)| {
					let core = core.0;
					let ids = ids.iter().map(|id| id.0).collect::<Vec<_>>();
					(core, ids)
				})
				.collect::<Vec<_>>()
		})
	}

	pub async fn get_session_index(&self, hash: H256) -> Result<Option<u32>, subxt::Error> {
		let addr = polkadot::storage().session().current_index();
		self.storage().at(hash).fetch(&addr).await
	}

	pub async fn get_session_index_now(&self) -> Result<Option<u32>, subxt::Error> {
		let addr = polkadot::storage().session().current_index();
		self.storage().at_latest().await?.fetch(&addr).await
	}

	pub async fn get_session_account_keys(&self, session_index: u32) -> Result<Option<Vec<AccountId32>>, subxt::Error> {
		let addr = polkadot::storage().para_session_info().account_keys(session_index);
		self.storage().at_latest().await?.fetch(&addr).await
	}

	pub async fn get_session_next_keys(&self, account: &AccountId32) -> Result<Option<SessionKeys>, subxt::Error> {
		let addr = polkadot::storage().session().next_keys(account);
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
			tokio::spawn(Self::get_hrmp_channels(self.storage(), block_hash, *sender, *para_id))
		});
		let inbound_channels: Vec<_> = join_requests(inbound_channels_fut).await?.into_iter().flatten().collect();

		let mut inbound_by_para_id: BTreeMap<u32, BTreeMap<u32, SubxtHrmpChannel>> = BTreeMap::new();
		for (sender, para_id, channel) in inbound_channels {
			let channels = inbound_by_para_id.entry(para_id).or_default();
			channels.insert(sender, channel.into());
		}

		let outbound_pairs = self.get_outbound_hrmp_channel_pairs(block_hash, para_ids.clone()).await?;
		let outbound_channels_fut = outbound_pairs.iter().map(|(para_id, recipient)| {
			tokio::spawn(Self::get_hrmp_channels(self.storage(), block_hash, *para_id, *recipient))
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
		let enter = ex
			.as_extrinsic::<polkadot::para_inherent::calls::types::Enter>()
			.map_err(|_| "Failed to decode `ParaInherent`".to_string())?
			.ok_or_else(|| "`ParaInherent` must exist".to_string())?;

		Ok(enter.data)
	}

	// We need it only for the historical mode to convert block numbers into their hashes
	pub async fn legacy_get_block_hash(
		&self,
		maybe_block_number: Option<BlockNumber>,
	) -> Result<Option<H256>, subxt::Error> {
		let maybe_block_number = maybe_block_number.map(|v| NumberOrHex::Number(v.into()));
		self.legacy_rpc_methods.chain_get_block_hash(maybe_block_number).await
	}

	pub async fn legacy_get_chain_name(&self) -> Result<String, subxt::Error> {
		self.legacy_rpc_methods.system_chain().await
	}

	pub async fn stream_best_block_headers(&self) -> Result<HeaderStream, subxt::Error> {
		self.client.backend().stream_best_block_headers().await
	}

	pub async fn stream_finalized_block_headers(&self) -> Result<HeaderStream, subxt::Error> {
		self.client.backend().stream_finalized_block_headers().await
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

	Ok(ApiClient { client, legacy_rpc_methods })
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
