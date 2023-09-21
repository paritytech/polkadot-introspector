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

use async_trait::async_trait;
use mockall::automock;
use polkadot_introspector_essentials::{
	api::subxt_wrapper::{RequestExecutor, SubxtHrmpChannel, SubxtWrapperError},
	metadata::polkadot_primitives::ValidatorIndex,
	types::{CoreOccupied, H256},
};
use std::collections::{BTreeMap, HashMap};

#[automock]
#[async_trait]
pub trait TrackerRpc {
	async fn inbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError>;
	async fn outbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError>;
	async fn core_assignments_via_scheduled_paras(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<HashMap<u32, Vec<u32>>>, SubxtWrapperError>;
	async fn core_assignments_via_claim_queue(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<HashMap<u32, Vec<u32>>>, SubxtWrapperError>;
	async fn backing_groups(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<Vec<Vec<ValidatorIndex>>>, SubxtWrapperError>;
	async fn block_timestamp(&mut self, block_hash: H256) -> color_eyre::Result<u64, SubxtWrapperError>;
	async fn occupied_cores(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<Vec<CoreOccupied>>, SubxtWrapperError>;
}

pub struct ParachainTrackerRpc {
	/// Parachain ID to track.
	para_id: u32,
	/// RPC node endpoint.
	node: String,
	/// A subxt API wrapper.
	executor: RequestExecutor,
}

impl ParachainTrackerRpc {
	pub fn new(para_id: u32, node: &str, executor: RequestExecutor) -> Self {
		Self { para_id, node: node.to_string(), executor }
	}
}

#[async_trait::async_trait]
impl TrackerRpc for ParachainTrackerRpc {
	async fn inbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		self.executor
			.get_inbound_hrmp_channels(self.node.as_str(), block_hash, self.para_id)
			.await
	}

	async fn outbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		self.executor
			.get_outbound_hrmp_channels(self.node.as_str(), block_hash, self.para_id)
			.await
	}

	async fn core_assignments_via_scheduled_paras(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<HashMap<u32, Vec<u32>>>, SubxtWrapperError> {
		Ok(self
			.executor
			.get_scheduled_paras(self.node.as_str(), block_hash)
			.await?
			.map(|v| v.iter().map(|v| (v.core.0, vec![v.para_id.0])).collect::<HashMap<_, _>>()))
	}

	async fn core_assignments_via_claim_queue(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<HashMap<u32, Vec<u32>>>, SubxtWrapperError> {
		Ok(self.executor.get_claim_queue(self.node.as_str(), block_hash).await?.map(|v| {
			v.iter()
				.map(|(core, queue)| {
					let ids = queue
						.iter()
						.filter_map(|v| v.as_ref().map(|v| v.assignment.para_id))
						.collect::<Vec<_>>();
					(*core, ids)
				})
				.collect()
		}))
	}

	async fn backing_groups(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<Vec<Vec<ValidatorIndex>>>, SubxtWrapperError> {
		self.executor.get_backing_groups(self.node.as_str(), block_hash).await
	}

	async fn block_timestamp(&mut self, block_hash: H256) -> color_eyre::Result<u64, SubxtWrapperError> {
		self.executor.get_block_timestamp(self.node.as_str(), block_hash).await
	}

	async fn occupied_cores(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Option<Vec<CoreOccupied>>, SubxtWrapperError> {
		self.executor.get_occupied_cores(self.node.as_str(), block_hash).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_utils::{create_api, rpc_node_url};
	use subxt::error::{Error, MetadataError};

	async fn setup_client() -> (ParachainTrackerRpc, H256) {
		let api = create_api();
		let rpc = ParachainTrackerRpc::new(100, rpc_node_url(), api.subxt());
		let block_hash = api.subxt().get_block(rpc_node_url(), None).await.unwrap().header().parent_hash;

		(rpc, block_hash)
	}

	#[tokio::test]
	async fn test_fetches_inbound_hrmp_channels() {
		let (mut rpc, block_hash) = setup_client().await;

		let response = rpc.inbound_hrmp_channels(block_hash).await;

		assert!(response.is_ok());
	}

	#[tokio::test]
	async fn test_fetches_outbound_hrmp_channels() {
		let (mut rpc, block_hash) = setup_client().await;

		let response = rpc.outbound_hrmp_channels(block_hash).await;

		assert!(response.is_ok());
	}

	#[tokio::test]
	async fn test_fetches_core_assignments_via_scheduled_paras() {
		let (mut rpc, block_hash) = setup_client().await;

		let response = rpc.core_assignments_via_scheduled_paras(block_hash).await;

		match response {
			Err(SubxtWrapperError::SubxtError(Error::Metadata(MetadataError::StorageEntryNotFound(reason)))) =>
				assert_eq!(reason, "Scheduled"),
			_ => assert!(response.is_ok()),
		};
	}

	#[tokio::test]
	async fn test_fetches_core_assignments_via_claim_queue() {
		let (mut rpc, block_hash) = setup_client().await;

		let response = rpc.core_assignments_via_claim_queue(block_hash).await;

		match response {
			Err(SubxtWrapperError::SubxtError(Error::Metadata(MetadataError::StorageEntryNotFound(reason)))) =>
				assert_eq!(reason, "ClaimQueue"),
			_ => assert!(response.is_ok()),
		};
	}

	#[tokio::test]
	async fn test_fetches_backing_groups() {
		let (mut rpc, block_hash) = setup_client().await;

		let response = rpc.backing_groups(block_hash).await;

		assert!(!response.unwrap().unwrap().is_empty());
	}

	#[tokio::test]
	async fn test_fetches_block_timestamp() {
		let (mut rpc, block_hash) = setup_client().await;

		let response = rpc.block_timestamp(block_hash).await;

		assert!(response.unwrap() > 0);
	}

	#[tokio::test]
	async fn test_fetches_occupied_cores() {
		let (mut rpc, block_hash) = setup_client().await;

		let response = rpc.occupied_cores(block_hash).await;

		assert!(!response.unwrap().unwrap().is_empty());
	}
}
