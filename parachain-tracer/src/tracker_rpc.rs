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
	api::subxt_wrapper::{RequestExecutor, SubxtHrmpChannel, SubxtWrapperError},
	metadata::polkadot_primitives::ValidatorIndex,
	types::{CoreOccupied, H256},
};
use std::collections::{BTreeMap, HashMap};

pub struct TrackerRpc {
	/// Parachain ID to track.
	para_id: u32,
	/// RPC node endpoint.
	node: String,
	/// A subxt API wrapper.
	executor: RequestExecutor,
}

impl TrackerRpc {
	pub fn new(para_id: u32, node: &str, executor: RequestExecutor) -> Self {
		Self { para_id, node: node.to_string(), executor }
	}

	pub async fn inbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		self.executor
			.get_inbound_hrmp_channels(self.node.as_str(), block_hash, self.para_id)
			.await
	}

	pub async fn outbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		self.executor
			.get_outbound_hrmp_channels(self.node.as_str(), block_hash, self.para_id)
			.await
	}

	pub async fn core_assignments_via_scheduled_paras(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<HashMap<u32, Vec<u32>>, SubxtWrapperError> {
		let core_assignments = self.executor.get_scheduled_paras(self.node.as_str(), block_hash).await?;

		Ok(core_assignments
			.iter()
			.map(|v| (v.core.0, vec![v.para_id.0]))
			.collect::<HashMap<_, _>>())
	}

	pub async fn core_assignments_via_claim_queue(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<HashMap<u32, Vec<u32>>, SubxtWrapperError> {
		let assignments = self.executor.get_claim_queue(self.node.as_str(), block_hash).await?;
		Ok(assignments
			.iter()
			.map(|(core, queue)| {
				let ids = queue
					.iter()
					.filter_map(|v| v.as_ref().map(|v| v.assignment.para_id))
					.collect::<Vec<_>>();
				(*core, ids)
			})
			.collect())
	}

	pub async fn backing_groups(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Vec<Vec<ValidatorIndex>>, SubxtWrapperError> {
		self.executor.get_backing_groups(self.node.as_str(), block_hash).await
	}

	pub async fn block_timestamp(&mut self, block_hash: H256) -> color_eyre::Result<u64, SubxtWrapperError> {
		self.executor.get_block_timestamp(self.node.as_str(), block_hash).await
	}

	pub async fn occupied_cores(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Vec<CoreOccupied>, SubxtWrapperError> {
		self.executor.get_occupied_cores(self.node.as_str(), block_hash).await
	}
}
