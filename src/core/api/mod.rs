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
#![allow(dead_code)]

use crate::core::{RecordsStorageConfig, MAX_MSG_QUEUE_SIZE};
use std::{fmt::Debug, hash::Hash};
use tokio::sync::mpsc::{channel, Sender};

mod storage;
mod subxt_wrapper;

pub use subxt_wrapper::*;

// Provides access to subxt and storage APIs, more to come.
#[derive(Clone)]
pub struct ApiService<K, P = ()> {
	storage_tx: Sender<storage::Request<K, P>>,
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
		RequestExecutor::new()
	}
}

// Unprefixed storage
impl<K> ApiService<K, ()>
where
	K: Eq + Sized + Hash + Debug + Clone + Send + 'static,
{
	pub fn new_with_storage(storage_config: RecordsStorageConfig) -> ApiService<K> {
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(storage::api_handler_task(storage_rx, storage_config));

		Self { storage_tx }
	}
}

// Prefixed storage
impl<K, P> ApiService<K, P>
where
	K: Eq + Sized + Hash + Debug + Clone + Send + Sync + 'static,
	P: Eq + Sized + Hash + Debug + Clone + Send + Sync + 'static,
{
	pub fn new_with_prefixed_storage(storage_config: RecordsStorageConfig) -> ApiService<K, P> {
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(storage::api_handler_task_prefixed(storage_rx, storage_config));

		Self { storage_tx }
	}
}
#[cfg(test)]
mod tests {
	use super::*;
	use crate::core::*;
	use subxt::{
		config::{substrate::BlakeTwo256, Hasher, Header},
		utils::H256,
	};
	#[cfg(feature = "polkadot")]
	const RPC_NODE_URL: &str = "wss://rpc.polkadot.io:443";
	#[cfg(feature = "rococo")]
	const RPC_NODE_URL: &str = "wss://rococo-rpc.polkadot.io:443";

	#[tokio::test]
	async fn basic_storage_test() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 10 });
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
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 10 });
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL, None).await.unwrap().unwrap();
		let timestamp = subxt.get_block_timestamp(RPC_NODE_URL, Some(head.hash())).await.unwrap();
		let _block = subxt.get_block(RPC_NODE_URL, Some(head.hash())).await.unwrap().unwrap();
		assert!(timestamp > 0);
	}

	#[tokio::test]
	async fn extract_parainherent_data() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let mut subxt = api.subxt();

		subxt
			.extract_parainherent_data(RPC_NODE_URL, None)
			.await
			.unwrap()
			.expect("Inherent data must be present");
	}

	#[tokio::test]
	async fn get_scheduled_paras() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL, None).await.unwrap().unwrap();

		assert!(!subxt.get_scheduled_paras(RPC_NODE_URL, head.hash()).await.unwrap().is_empty())
	}

	#[tokio::test]
	async fn get_occupied_cores() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL, None).await.unwrap().unwrap();

		assert!(!subxt.get_occupied_cores(RPC_NODE_URL, head.hash()).await.unwrap().is_empty())
	}

	#[tokio::test]
	async fn get_backing_groups() {
		let api = ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let mut subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL, None).await.unwrap().unwrap();

		assert!(!subxt.get_backing_groups(RPC_NODE_URL, head.hash()).await.unwrap().is_empty())
	}
}
