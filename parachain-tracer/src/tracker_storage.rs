// Copyright 2023 Parity Technologies (UK) Ltd.
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

use polkadot_introspector_essentials::{
	api::storage::RequestExecutor,
	collector::{candidate_record::CandidateRecord, CollectorPrefixType, DisputeInfo},
	metadata::polkadot_primitives::ValidatorIndex,
	types::{AccountId32, CoreOccupied, InherentData, OnDemandOrder, Timestamp, H256},
};
use std::collections::BTreeMap;
use subxt::config::{substrate::BlakeTwo256, Hasher};

pub struct TrackerStorage {
	/// Parachain ID to track.
	para_id: u32,
	/// API to access collector's storage
	storage: RequestExecutor<H256, CollectorPrefixType>,
}

impl TrackerStorage {
	pub fn new(para_id: u32, storage: RequestExecutor<H256, CollectorPrefixType>) -> Self {
		Self { para_id, storage }
	}

	/// Reads validators account keys for the given session index
	pub async fn session_keys(&self, session_index: u32) -> Option<Vec<AccountId32>> {
		self.storage
			.storage_read_prefixed(
				CollectorPrefixType::AccountKeys,
				BlakeTwo256::hash(&session_index.to_be_bytes()[..]),
			)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Reads inherent data of a relay block by its block hash
	pub async fn inherent_data(&self, block_hash: H256) -> Option<InherentData> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::InherentData, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Reads on-demand order information by para id and block hash when it was placed
	pub async fn on_demand_order(&self, block_hash: H256) -> Option<OnDemandOrder> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::OnDemandOrder(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Reads the last finalized block number at the moment, when the given block has appeared
	pub async fn relevant_finalized_block_number(&self, block_hash: H256) -> Option<u32> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::RelevantFinalizedBlockNumber, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Read the dispute info for the given block by parachain id
	pub async fn dispute(&self, block_hash: H256) -> Option<DisputeInfo> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::Dispute(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Read the candidate info for the given parablock by parachain id
	pub async fn candidate(&self, candidate_hash: H256) -> Option<CandidateRecord> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::Candidate(self.para_id), candidate_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Read the timestamp for the given relay block
	pub async fn block_timestamp(&self, block_hash: H256) -> Option<Timestamp> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::Timestamp, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Read the occupied cores for the given relay block
	pub async fn occupied_cores(&self, block_hash: H256) -> Option<Vec<CoreOccupied>> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::OccupiedCores, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Read the backing groups for the given relay block
	pub async fn backing_groups(&self, block_hash: H256) -> Option<Vec<Vec<ValidatorIndex>>> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::BackingGroups, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Read the core assignments for the given relay block
	pub async fn core_assignments(&self, block_hash: H256) -> Option<BTreeMap<u32, Vec<u32>>> {
		self.storage
			.storage_read_prefixed(CollectorPrefixType::CoreAssignments, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}
}

#[cfg(test)]
mod tests {
	use crate::test_utils::{create_candidate_record, create_inherent_data};

	use super::*;
	use polkadot_introspector_essentials::{
		api::ApiService,
		chain_events::SubxtDispute,
		collector::CollectorStorageApi,
		storage::{RecordTime, RecordsStorageConfig, StorageEntry},
	};
	use std::time::Duration;
	use subxt::utils::AccountId32;

	fn setup_client() -> (TrackerStorage, CollectorStorageApi) {
		let api: CollectorStorageApi =
			ApiService::new_with_prefixed_storage(RecordsStorageConfig { max_blocks: 4 }, Default::default());
		let storage = TrackerStorage::new(100, api.storage());

		(storage, api)
	}

	#[tokio::test]
	async fn test_reads_session_keys() {
		let (storage, api) = setup_client();
		let keys = vec![AccountId32([42; 32])];
		let session_index: u32 = 42;
		let session_hash = BlakeTwo256::hash(&session_index.to_be_bytes()[..]);
		assert!(storage.session_keys(session_index).await.is_none());

		api.storage()
			.storage_write_prefixed(
				CollectorPrefixType::AccountKeys,
				session_hash,
				StorageEntry::new_persistent(RecordTime::with_ts(100, Duration::from_secs(0)), keys.clone()),
			)
			.await
			.unwrap();

		assert_eq!(storage.session_keys(session_index).await, Some(keys));
	}

	#[tokio::test]
	async fn test_reads_inherent_data() {
		let (storage, api) = setup_client();
		let hash = H256::random();
		assert!(storage.inherent_data(hash).await.is_none());

		let data = create_inherent_data(100);
		let parent_hash = data.parent_header.parent_hash;
		api.storage()
			.storage_write_prefixed(
				CollectorPrefixType::InherentData,
				hash,
				StorageEntry::new_onchain(RecordTime::with_ts(0, Duration::from_secs(0)), data),
			)
			.await
			.unwrap();

		let storage_data = storage.inherent_data(hash).await.unwrap();
		assert_eq!(storage_data.parent_header.parent_hash, parent_hash);
	}

	#[tokio::test]
	async fn test_reads_on_demand_order() {
		let (storage, api) = setup_client();
		let hash = H256::random();
		assert!(storage.on_demand_order(hash).await.is_none());

		api.storage()
			.storage_write_prefixed(
				CollectorPrefixType::OnDemandOrder(100),
				hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(0, Duration::from_secs(0)),
					OnDemandOrder { para_id: 100, spot_price: 1 },
				),
			)
			.await
			.unwrap();

		let storage_order = storage.on_demand_order(hash).await.unwrap();
		assert_eq!(storage_order.para_id, 100);
		assert_eq!(storage_order.spot_price, 1);
	}

	#[tokio::test]
	async fn test_reads_relevant_finalized_block_number() {
		let (storage, api) = setup_client();
		let hash = H256::random();
		assert!(storage.relevant_finalized_block_number(hash).await.is_none());

		api.storage()
			.storage_write_prefixed(
				CollectorPrefixType::RelevantFinalizedBlockNumber,
				hash,
				StorageEntry::new_onchain(RecordTime::with_ts(0, Duration::from_secs(0)), 42),
			)
			.await
			.unwrap();

		assert_eq!(storage.relevant_finalized_block_number(hash).await.unwrap(), 42);
	}

	#[tokio::test]
	async fn test_reads_dispute() {
		let (storage, api) = setup_client();
		let hash = H256::random();
		assert!(storage.dispute(hash).await.is_none());

		api.storage()
			.storage_write_prefixed(
				CollectorPrefixType::Dispute(100),
				hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(0, Duration::from_secs(0)),
					DisputeInfo {
						initiated: 1000,
						initiator_indices: vec![42],
						session_index: 1000,
						dispute: SubxtDispute { relay_parent_block: H256::random(), candidate_hash: hash },
						parachain_id: 100,
						outcome: None,
						concluded: None,
					},
				),
			)
			.await
			.unwrap();

		let storage_dispute = storage.dispute(hash).await.unwrap();
		assert_eq!(storage_dispute.initiated, 1000);
		assert_eq!(storage_dispute.initiator_indices, vec![42]);
		assert_eq!(storage_dispute.session_index, 1000);
		assert_eq!(storage_dispute.parachain_id, 100);
	}

	#[tokio::test]
	async fn test_reads_candidate() {
		let (storage, api) = setup_client();
		let hash = H256::random();
		assert!(storage.candidate(hash).await.is_none());

		api.storage()
			.storage_write_prefixed(
				CollectorPrefixType::Candidate(100),
				hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(0, Duration::from_secs(0)),
					create_candidate_record(100, 0, H256::random(), 0),
				),
			)
			.await
			.unwrap();

		let storage_record = storage.candidate(hash).await.unwrap();
		assert_eq!(storage_record.candidate_inclusion.parachain_id, 100);
		assert_eq!(storage_record.candidate_first_seen, Duration::from_secs(0));
		assert!(storage_record.candidate_disputed.is_none());
	}
}
