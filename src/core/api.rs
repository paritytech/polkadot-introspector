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

use subxt::DefaultConfig;

use tokio::sync::{mpsc::Sender, oneshot};

#[derive(Debug)]
pub struct Request {
	pub url: String,
	pub request_type: RequestType,
	pub response_sender: oneshot::Sender<Response>,
}

pub struct RequestExecutor {
	to_api: Sender<Request>,
}

impl RequestExecutor {
	pub fn new(to_api: Sender<Request>) -> Self {
		RequestExecutor { to_api }
	}

	pub async fn get_block_timestamp(&self, url: String, hash: Option<<DefaultConfig as subxt::Config>::Hash>) -> u64 {
		let (sender, receiver) = oneshot::channel::<Response>();
		let request = Request { url, request_type: RequestType::GetBlockTimestamp(hash), response_sender: sender };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::GetBlockTimestampResponse(ts)) => ts,
			_ => panic!("Expected GetBlockTimestampResponse, got something else."),
		}
	}

	pub async fn get_block_head(
		&self,
		url: String,
		hash: Option<<DefaultConfig as subxt::Config>::Hash>,
	) -> Option<<DefaultConfig as subxt::Config>::Header> {
		let (sender, receiver) = oneshot::channel::<Response>();
		let request = Request { url, request_type: RequestType::GetHead(hash), response_sender: sender };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::GetHeadResponse(maybe_head)) => maybe_head,
			_ => panic!("Expected GetHeadResponse, got something else."),
		}
	}
}

#[derive(Clone, Debug)]
pub enum RequestType {
	GetBlockTimestamp(Option<<DefaultConfig as subxt::Config>::Hash>),
	GetHead(Option<<DefaultConfig as subxt::Config>::Hash>),
}

#[derive(Debug)]
pub enum Response {
	GetBlockTimestampResponse(u64),
	GetHeadResponse(Option<<DefaultConfig as subxt::Config>::Header>),
}

#[derive(Debug)]
pub enum Error {
	SubxtError(subxt::BasicError),
}

pub type Result = std::result::Result<Response, Error>;
