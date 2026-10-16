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

pub mod api_client;
pub mod decode;
pub mod dynamic;
pub mod executor;
pub mod shadow;
pub mod storage;

use crate::{api::executor::RequestExecutor, constants::MAX_MSG_QUEUE_SIZE, storage::RecordsStorageConfig};
use std::{fmt::Debug, hash::Hash};
use tokio::sync::mpsc::{Sender, channel};

// Provides access to subxt and storage APIs, more to come.
#[derive(Clone)]
pub struct ApiService<K, P = ()> {
	storage_tx: Sender<storage::Request<K, P>>,
	executor: RequestExecutor,
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

	pub fn executor(&self) -> RequestExecutor {
		self.executor.clone()
	}
}

// Unprefixed storage
impl<K> ApiService<K, ()>
where
	K: Eq + Sized + Hash + Debug + Clone + Send + 'static,
{
	pub fn new_with_storage(storage_config: RecordsStorageConfig, executor: RequestExecutor) -> ApiService<K> {
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(storage::api_handler_task(storage_rx, storage_config));

		Self { storage_tx, executor }
	}
}

// Prefixed storage
impl<K, P> ApiService<K, P>
where
	K: Eq + Sized + Hash + Debug + Clone + Send + Sync + 'static,
	P: Eq + Sized + Hash + Debug + Clone + Send + Sync + 'static,
{
	pub fn new_with_prefixed_storage(
		storage_config: RecordsStorageConfig,
		executor: RequestExecutor,
	) -> ApiService<K, P> {
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(storage::api_handler_task_prefixed(storage_rx, storage_config));

		Self { storage_tx, executor }
	}
}
#[cfg(test)]
mod tests {
	use super::*;
	use crate::{api::api_client::ApiClientMode, init, storage::StorageEntry, types::H256, utils::RetryOptions};
	use subxt::config::Hasher;

	fn rpc_node_url() -> &'static str {
		const RPC_NODE_URL: &str = "wss://rpc.polkadot.io:443";

		if let Ok(url) = std::env::var("WS_URL") {
			return Box::leak(url.into_boxed_str())
		}

		RPC_NODE_URL
	}

	async fn request_executor() -> RequestExecutor {
		let shutdown_tx = init::init_shutdown();
		RequestExecutor::build(rpc_node_url(), ApiClientMode::RPC, &RetryOptions::default(), &shutdown_tx, false)
			.await
			.unwrap()
	}

	#[tokio::test]
	async fn basic_storage_test() {
		let executor = request_executor().await;
		let hasher = executor.hasher(rpc_node_url()).unwrap();
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 10 }, executor);
		let storage = api.storage();
		let key = hasher.hash_of(&100);
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
			ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 10 }, request_executor().await);
		let mut subxt = api.executor();
		let hasher = subxt.hasher(rpc_node_url()).unwrap();

		let head = subxt.get_block_head(rpc_node_url(), None).await.unwrap().unwrap();
		let timestamp = subxt.get_block_timestamp(rpc_node_url(), hasher.hash_of(&head)).await.unwrap();
		let _block = subxt
			.get_block_number(rpc_node_url(), Some(hasher.hash_of(&head)))
			.await
			.unwrap();
		assert!(timestamp > 0);
	}

	#[tokio::test]
	async fn extract_parainherent_data() {
		let api =
			ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, request_executor().await);
		let mut subxt = api.executor();

		subxt
			.extract_parainherent_data(rpc_node_url(), None)
			.await
			.expect("Inherent data must be present");
	}

	#[tokio::test]
	async fn get_occupied_cores() {
		let api =
			ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, request_executor().await);
		let mut subxt = api.executor();
		let hasher = subxt.hasher(rpc_node_url()).unwrap();

		let head = subxt.get_block_head(rpc_node_url(), None).await.unwrap().unwrap();
		let cores = subxt.get_occupied_cores(rpc_node_url(), hasher.hash_of(&head)).await;
		assert!(cores.is_ok());
	}

	#[tokio::test]
	async fn get_backing_groups() {
		let api =
			ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, request_executor().await);
		let mut subxt = api.executor();
		let hasher = subxt.hasher(rpc_node_url()).unwrap();

		let head = subxt.get_block_head(rpc_node_url(), None).await.unwrap().unwrap();
		let groups = subxt.get_backing_groups(rpc_node_url(), hasher.hash_of(&head)).await;

		assert!(groups.is_ok());
	}

	/// Disputes seen on Polkadot, as `(relay block, candidate hashes in the block's inherent)`.
	const ARCHIVE_DISPUTE_BLOCKS: &[(u32, &[&str])] = &[
		(
			33110743,
			&[
				"3bb9910e2302f7dced46935b30fce765e971ab7541061f2e20d4e3b5b8ce76d3",
				"7fdc6938fc8d047910f40a4585350277721172d2466b0877f8576e5e124cfffd",
				"44d7c961ed652e64277855be0e9f763af0349786e08e88a71eba037130fa1af9",
			],
		),
		(33110773, &["df8aa2ad390b3870ae1cd0860ef88bf822072c69e17c47bb1bbfabf47ea29e82"]),
	];

	/// Real ParaInherent disputes must decode through both paths. The client runs with shadow decoding
	/// on, so the metadata-free decode is compared against the metadata one for every read, and any
	/// mismatch aborts the process; the assertions then pin the disputes to the known candidates.
	///
	/// Disputes are rare and only archive nodes keep old state, so this is opt-in. Point
	/// `ARCHIVE_WS_URL` at a Polkadot archive node, e.g. `wss://rpc.polkadot.io:443`, which serves
	/// these blocks:
	/// `ARCHIVE_WS_URL=wss://rpc.polkadot.io:443 cargo test -p polkadot-introspector-essentials \
	///  archive_inherent_disputes -- --ignored`
	#[tokio::test]
	#[ignore = "needs a Polkadot archive node in ARCHIVE_WS_URL"]
	async fn archive_inherent_disputes_decode() {
		// A shadow mismatch logs the diff and then exits, so the logger must be up to see it.
		let _ = env_logger::try_init();
		let url: &'static str = Box::leak(
			std::env::var("ARCHIVE_WS_URL")
				.expect("set ARCHIVE_WS_URL to a Polkadot archive node")
				.into_boxed_str(),
		);
		let shutdown_tx = init::init_shutdown();
		let executor = RequestExecutor::build(url, ApiClientMode::RPC, &RetryOptions::default(), &shutdown_tx, true)
			.await
			.unwrap();
		let mut subxt =
			ApiService::<H256>::new_with_storage(RecordsStorageConfig { max_blocks: 1 }, executor).executor();

		for (number, expected) in ARCHIVE_DISPUTE_BLOCKS {
			let hash = subxt
				.get_block_hash(url, Some(*number))
				.await
				.unwrap()
				.unwrap_or_else(|| panic!("block {number} not found; is this an archive node?"));
			let inherent = subxt.extract_parainherent_data(url, Some(hash)).await.unwrap();

			for candidate in *expected {
				let candidate = H256::from_slice(&hex::decode(candidate).unwrap());
				let set = inherent
					.disputes
					.iter()
					.find(|set| set.candidate_hash.0 == candidate)
					.unwrap_or_else(|| panic!("block {number}: dispute for {candidate:?} not decoded"));
				assert!(!set.statements.is_empty(), "block {number}: dispute {candidate:?} has no statements");
			}
		}
	}
}
