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
	api::subxt_wrapper::InherentData,
	collector::{candidate_record::CandidateRecord, CollectorPrefixType, CollectorStorageApi, DisputeInfo},
	types::{AccountId32, OnDemandOrder, H256},
};
use subxt::config::{substrate::BlakeTwo256, Hasher};

pub struct TrackerStorage {
	/// Parachain ID to track.
	para_id: u32,
	/// API to access collector's storage
	api: CollectorStorageApi,
}

impl TrackerStorage {
	pub fn new(para_id: u32, api: CollectorStorageApi) -> Self {
		Self { para_id, api }
	}

	pub async fn session_keys(&self, session_index: u32) -> Option<Vec<AccountId32>> {
		self.api
			.storage()
			.storage_read_prefixed(
				CollectorPrefixType::AccountKeys,
				BlakeTwo256::hash(&session_index.to_be_bytes()[..]),
			)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub async fn inherent_data(&self, block_hash: H256) -> Option<InherentData> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::InherentData, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub async fn on_demand_order(&self, block_hash: H256) -> Option<OnDemandOrder> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::OnDemandOrder(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub async fn relevant_finalized_block_number(&self, block_hash: H256) -> Option<u32> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::RelevantFinalizedBlockNumber, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub async fn dispute(&self, block_hash: H256) -> Option<DisputeInfo> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::Dispute(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub async fn candidate(&self, candidate_hash: H256) -> Option<CandidateRecord> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::Candidate(self.para_id), candidate_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use polkadot_introspector_essentials::{
		api::ApiService,
		chain_events::SubxtDispute,
		collector::candidate_record::CandidateInclusionRecord,
		metadata::polkadot::runtime_types::sp_runtime::generic::{digest::Digest, header::Header},
		storage::{RecordTime, RecordsStorageConfig, StorageEntry},
	};
	use std::time::Duration;
	use subxt::utils::AccountId32;

	fn setup_client() -> (TrackerStorage, CollectorStorageApi) {
		let api: CollectorStorageApi =
			ApiService::new_with_prefixed_storage(RecordsStorageConfig { max_blocks: 4 }, Default::default());
		let storage = TrackerStorage::new(100, api.clone());

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
		let parent_hash = H256::random();
		let hash = H256::random();
		assert!(storage.inherent_data(hash).await.is_none());

		api.storage()
			.storage_write_prefixed(
				CollectorPrefixType::InherentData,
				hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(0, Duration::from_secs(0)),
					InherentData {
						bitfields: Default::default(),
						backed_candidates: Default::default(),
						disputes: Default::default(),
						parent_header: Header {
							parent_hash,
							number: Default::default(),
							state_root: Default::default(),
							extrinsics_root: Default::default(),
							digest: Digest { logs: Default::default() },
							__subxt_unused_type_params: Default::default(),
						},
					},
				),
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
					CandidateRecord {
						candidate_inclusion: CandidateInclusionRecord {
							parachain_id: 100,
							backed: 0,
							included: None,
							timedout: None,
							core_idx: None,
							relay_parent: H256::random(),
							relay_parent_number: 0,
						},
						candidate_first_seen: Duration::from_secs(0),
						candidate_disputed: None,
					},
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
