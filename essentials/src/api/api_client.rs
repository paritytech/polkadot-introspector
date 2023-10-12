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

use crate::types::{BlockNumber, H256};
use subxt::{
	backend::{
		legacy::{rpc_methods::NumberOrHex, LegacyRpcMethods},
		rpc::RpcClient,
		StreamOf,
	},
	blocks::{BlockRef, BlocksClient},
	config::substrate::{BlakeTwo256, SubstrateHeader},
	events::EventsClient,
	storage::StorageClient,
	OnlineClient, PolkadotConfig,
};

#[derive(Clone)]
pub struct ApiClient {
	client: OnlineClient<PolkadotConfig>,
	legacy_rpc_methods: LegacyRpcMethods<PolkadotConfig>,
}

pub type HeaderStream = StreamOf<Result<(SubstrateHeader<u32, BlakeTwo256>, BlockRef<H256>), subxt::Error>>;

impl ApiClient {
	pub async fn build(url: &str) -> Result<ApiClient, String> {
		let rpc_client = RpcClient::from_url(url)
			.await
			.map_err(|e| format!("Cannot construct RPC client: {e}"))?;
		let client = OnlineClient::from_rpc_client(rpc_client.clone())
			.await
			.map_err(|e| format!("Cannot construct OnlineClient from rpc client: {e}"))?;
		let legacy_rpc_methods = LegacyRpcMethods::<PolkadotConfig>::new(rpc_client);

		Ok(ApiClient { client, legacy_rpc_methods })
	}

	pub fn storage(&self) -> StorageClient<PolkadotConfig, OnlineClient<PolkadotConfig>> {
		self.client.storage()
	}

	pub fn blocks(&self) -> BlocksClient<PolkadotConfig, OnlineClient<PolkadotConfig>> {
		self.client.blocks()
	}

	pub fn events(&self) -> EventsClient<PolkadotConfig, OnlineClient<PolkadotConfig>> {
		self.client.events()
	}

	pub async fn legacy_get_block_hash(
		&self,
		maybe_block_number: Option<BlockNumber>,
	) -> Result<Option<H256>, subxt::Error> {
		let maybe_block_number = maybe_block_number.map(|v| NumberOrHex::Number(v.into()));
		self.legacy_rpc_methods.chain_get_block_hash(maybe_block_number).await
	}

	pub async fn stream_best_block_headers(&self) -> Result<HeaderStream, subxt::Error> {
		self.client.backend().stream_best_block_headers().await
	}

	pub async fn stream_finalized_block_headers(&self) -> Result<HeaderStream, subxt::Error> {
		self.client.backend().stream_finalized_block_headers().await
	}
}
