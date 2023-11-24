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
	api::subxt_wrapper::{RequestExecutor, SubxtWrapperError},
	types::{SubxtHrmpChannel, H256},
};
use std::collections::BTreeMap;

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
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_utils::{create_api, rpc_node_url};

	async fn setup_client() -> (ParachainTrackerRpc, H256) {
		let api = create_api();
		let rpc = ParachainTrackerRpc::new(100, rpc_node_url(), api.subxt());
		let block_hash = api.subxt().get_block_hash(rpc_node_url(), None).await.unwrap().unwrap();

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
}
