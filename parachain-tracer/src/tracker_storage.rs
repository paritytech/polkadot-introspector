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
	pub(crate) fn new(para_id: u32, api: CollectorStorageApi) -> Self {
		Self { para_id, api }
	}

	pub(crate) async fn session_keys(&self, session_index: u32) -> Option<Vec<AccountId32>> {
		self.api
			.storage()
			.storage_read_prefixed(
				CollectorPrefixType::AccountKeys,
				BlakeTwo256::hash(&session_index.to_be_bytes()[..]),
			)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub(crate) async fn inherent_data(&self, block_hash: H256) -> Option<InherentData> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::InherentData, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub(crate) async fn on_demand_order(&self, block_hash: H256) -> Option<OnDemandOrder> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::OnDemandOrder(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub(crate) async fn relevant_finalized_block_number(&self, block_hash: H256) -> Option<u32> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::RelevantFinalizedBlockNumber, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub(crate) async fn dispute(&self, block_hash: H256) -> Option<DisputeInfo> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::Dispute(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	pub(crate) async fn candidate(&self, candidate_hash: H256) -> Option<CandidateRecord> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::Candidate(self.para_id), candidate_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}
}
