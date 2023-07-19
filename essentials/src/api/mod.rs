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

mod dynamic;
mod storage;
pub mod subxt_wrapper;

use crate::{constants::MAX_MSG_QUEUE_SIZE, storage::RecordsStorageConfig, utils::RetryOptions};
use std::{fmt::Debug, hash::Hash};
use subxt_wrapper::RequestExecutor;
use tokio::sync::mpsc::{channel, Sender};

// Provides access to subxt and storage APIs, more to come.
#[derive(Clone)]
pub struct ApiService<K, P = ()> {
	storage_tx: Sender<storage::Request<K, P>>,
	retry: RetryOptions,
}

// Common methods
impl<K, P> ApiService<K, P>
where
	K: Debug,
	P: Debug,
{
	pub fn storage(&self) -> storage::RequestExecutor<K, P> {
		storage::RequestExecutor::new(self.storage_tx.clone())
	}

	pub fn subxt(&self) -> subxt_wrapper::RequestExecutor {
		RequestExecutor::new(self.retry.clone())
	}
}

// Unprefixed storage
impl<K> ApiService<K, ()>
where
	K: Eq + Sized + Hash + Debug + Clone + Send + 'static,
{
	pub fn new_with_storage(storage_config: RecordsStorageConfig, retry: RetryOptions) -> ApiService<K> {
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(storage::api_handler_task(storage_rx, storage_config));

		Self { storage_tx, retry }
	}
}

// Prefixed storage
impl<K, P> ApiService<K, P>
where
	K: Eq + Sized + Hash + Debug + Clone + Send + Sync + 'static,
	P: Eq + Sized + Hash + Debug + Clone + Send + Sync + 'static,
{
	pub fn new_with_prefixed_storage(storage_config: RecordsStorageConfig, retry: RetryOptions) -> ApiService<K, P> {
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(storage::api_handler_task_prefixed(storage_rx, storage_config));

		Self { storage_tx, retry }
	}
}
#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		metadata::{
			polkadot::runtime_types::polkadot_runtime_parachains::scheduler::CoreAssignment,
			polkadot_primitives::CoreOccupied,
		},
		storage::StorageEntry,
		types::H256,
	};
	use subxt::config::{substrate::BlakeTwo256, Hasher, Header};

	fn rpc_node_url() -> &'static str {
		#[cfg(feature = "rococo")]
		const RPC_NODE_URL: &str = "wss://rococo-rpc.polkadot.io:443";
		#[cfg(feature = "kusama")]
		const RPC_NODE_URL: &str = "wss://kusama-rpc.polkadot.io:443";
		#[cfg(feature = "polkadot")]
		const RPC_NODE_URL: &str = "wss://rpc.polkadot.io:443";

		if let Ok(url) = std::env::var("WS_URL") {
			return Box::leak(url.into_boxed_str())
		}

		RPC_NODE_URL
	}

	#[tokio::test]
	async fn basic_storage_test() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 10 }, RetryOptions::default());
		let storage = api.storage();
		let key = BlakeTwo256::hash_of(&100);
		storage
			.storage_write(key, StorageEntry::new_onchain(1.into(), "some data"))
			.await
			.unwrap();
		let value = storage.storage_read(key).await.unwrap();
		assert_eq!(value.into_inner::<String>().unwrap(), "some data");
	}

	#[tokio::test]
	async fn basic_subxt_test() {
		let api =
			ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 10 }, RetryOptions::default());
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(rpc_node_url(), None).await.unwrap().unwrap();
		let timestamp = subxt.get_block_timestamp(rpc_node_url(), head.hash()).await.unwrap();
		let _block = subxt.get_block(rpc_node_url(), Some(head.hash())).await.unwrap().unwrap();
		assert!(timestamp > 0);
	}

	#[tokio::test]
	async fn extract_parainherent_data() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, RetryOptions::default());
		let mut subxt = api.subxt();

		subxt
			.extract_parainherent_data(rpc_node_url(), None)
			.await
			.unwrap()
			.expect("Inherent data must be present");
	}

	#[tokio::test]
	async fn get_scheduled_paras() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, RetryOptions::default());
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(rpc_node_url(), None).await.unwrap().unwrap();
		let paras = subxt.get_scheduled_paras(rpc_node_url(), head.hash()).await.unwrap();

		assert!(!paras.is_empty());
		assert!(matches!(paras[0], CoreAssignment { .. }));
	}

	#[tokio::test]
	async fn get_occupied_cores() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, RetryOptions::default());
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(rpc_node_url(), None).await.unwrap().unwrap();
		let cores = subxt.get_occupied_cores(rpc_node_url(), head.hash()).await.unwrap();

		assert!(!cores.is_empty());
		assert!(matches!(cores[0], Some(CoreOccupied::Parachain)) || matches!(cores[0], None))
	}

	#[tokio::test]
	async fn get_backing_groups() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, RetryOptions::default());
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(rpc_node_url(), None).await.unwrap().unwrap();
		let groups = subxt.get_backing_groups(rpc_node_url(), head.hash()).await.unwrap();

		assert!(!groups.is_empty());
		assert_eq!(groups[0][0].0, 0); // First validator's index is 0
	}
}
