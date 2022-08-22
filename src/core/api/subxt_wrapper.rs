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
use crate::core::subxt_subscription::polkadot;
use codec::{Compact, Decode, Encode};
use log::error;

use crate::core::subxt_subscription::polkadot::{
	runtime_types as subxt_runtime_types, runtime_types::polkadot_primitives as polkadot_rt_primitives,
};
pub use subxt_runtime_types::polkadot_runtime::Call as SubxtCall;

use std::collections::hash_map::{Entry, HashMap};
use subxt::{sp_core::H256, ClientBuilder, DefaultConfig, PolkadotExtrinsicParams};

use tokio::sync::{
	mpsc::{Receiver, Sender},
	oneshot,
	oneshot::error::TryRecvError,
};

/// Subxt based APIs for fetching via RPC and processing of extrinsics.
#[derive(Clone, Debug)]
pub enum RequestType {
	/// Returns the block TS from the inherent data.
	GetBlockTimestamp(Option<<DefaultConfig as subxt::Config>::Hash>),
	/// Get a block header.
	GetHead(Option<<DefaultConfig as subxt::Config>::Hash>),
	/// Get a full block.
	GetBlock(Option<<DefaultConfig as subxt::Config>::Hash>),
	/// Extract the `ParaInherentData` from a given block.
	ExtractParaInherent(subxt::rpc::ChainBlock<DefaultConfig>),
}

/// The `InherentData` constructed with the subxt API.
type InherentData = polkadot_rt_primitives::v2::InherentData<
	subxt_runtime_types::sp_runtime::generic::header::Header<
		::core::primitive::u32,
		subxt_runtime_types::sp_runtime::traits::BlakeTwo256,
	>,
>;

/// Response types for APIs.
#[derive(Debug)]
pub enum Response {
	/// A timestamp.
	Timestamp(u64),
	/// A block heahder.
	MaybeHead(Option<<DefaultConfig as subxt::Config>::Header>),
	/// A full block.
	MaybeBlock(Option<subxt::rpc::ChainBlock<DefaultConfig>>),
	/// `ParaInherent` data.
	ParaInherentData(InherentData),
}

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
			Ok(Response::Timestamp(ts)) => ts,
			_ => panic!("Expected Timestamp, got something else."),
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
			Ok(Response::MaybeHead(maybe_head)) => maybe_head,
			_ => panic!("Expected MaybeHead, got something else."),
		}
	}

	pub async fn get_block(
		&self,
		url: String,
		hash: Option<<DefaultConfig as subxt::Config>::Hash>,
	) -> Option<subxt::rpc::ChainBlock<DefaultConfig>> {
		let (sender, receiver) = oneshot::channel::<Response>();
		let request = Request { url, request_type: RequestType::GetBlock(hash), response_sender: sender };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::MaybeBlock(maybe_block)) => maybe_block,
			_ => panic!("Expected MaybeHead, got something else."),
		}
	}

	pub async fn extract_parainherent_data(
		&self,
		url: String,
		maybe_hash: Option<<DefaultConfig as subxt::Config>::Hash>,
	) -> Option<InherentData> {
		if let Some(block) = self.get_block(url.clone(), maybe_hash).await {
			let (sender, receiver) = oneshot::channel::<Response>();
			let request =
				Request { url, request_type: RequestType::ExtractParaInherent(block), response_sender: sender };
			self.to_api.send(request).await.expect("Channel closed");

			match receiver.await {
				Ok(Response::ParaInherentData(data)) => Some(data),
				_ => panic!("Expected MaybeHead, got something else."),
			}
		} else {
			None
		}
	}
}

// Attempts to connect to websocket and returns an RuntimeApi instance if successful.
async fn new_client_fn(
	url: String,
) -> Option<polkadot::RuntimeApi<DefaultConfig, PolkadotExtrinsicParams<DefaultConfig>>> {
	for _ in 0..crate::core::RETRY_COUNT {
		match ClientBuilder::new().set_url(url.clone()).build().await {
			Ok(api) =>
				return Some(
					api.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, PolkadotExtrinsicParams<DefaultConfig>>>(),
				),
			Err(err) => {
				error!("[{}] Client error: {:?}", url, err);
				tokio::time::sleep(std::time::Duration::from_millis(crate::core::RETRY_DELAY_MS)).await;
				continue
			},
		};
	}
	None
}
// A task that handles subxt API calls.
pub(crate) async fn api_handler_task(mut api: Receiver<Request>) {
	let mut connection_pool = HashMap::new();
	loop {
		if let Some(request) = api.recv().await {
			let (timeout_sender, mut timeout_receiver) = oneshot::channel::<bool>();

			// Start API retry timeout task.
			let timeout_task = tokio::spawn(async move {
				tokio::time::sleep(std::time::Duration::from_millis(crate::core::API_RETRY_TIMEOUT_MS)).await;
				timeout_sender.send(true).expect("Sending timeout signal never fails.");
			});

			// Loop while the timeout task doesn't fire. Other errors will cancel this loop
			loop {
				match timeout_receiver.try_recv() {
					Err(TryRecvError::Empty) => {},
					Err(TryRecvError::Closed) => {
						// Timeout task has exit unexpectedely. this shuld never happen.
						panic!("API timeout task closed channel: {:?}", request);
					},
					Ok(_) => {
						// Panic on timeout.
						panic!("Request timed out: {:?}", request);
					},
				}
				match connection_pool.entry(request.url.clone()) {
					Entry::Occupied(_) => (),
					Entry::Vacant(entry) => {
						let maybe_api = new_client_fn(request.url.clone()).await;
						if let Some(api) = maybe_api {
							entry.insert(api);
						}
					},
				};

				let api = connection_pool.get(&request.url.clone());

				let result = if let Some(api) = api {
					match request.request_type {
						RequestType::GetBlockTimestamp(maybe_hash) => subxt_get_block_ts(api, maybe_hash).await,
						RequestType::GetHead(maybe_hash) => subxt_get_head(api, maybe_hash).await,
						RequestType::GetBlock(maybe_hash) => subxt_get_block(api, maybe_hash).await,
						RequestType::ExtractParaInherent(ref block) => subxt_extract_parainherent(block),
					}
				} else {
					// Remove the faulty websocket from connection pool.
					let _ = connection_pool.remove(&request.url);
					tokio::time::sleep(std::time::Duration::from_millis(crate::core::RETRY_DELAY_MS)).await;
					continue
				};

				let response = match result {
					Ok(response) => response,
					Err(Error::SubxtError(err)) => {
						error!("subxt call error: {:?}", err);
						// Always retry for subxt errors (most of them are transient).
						let _ = connection_pool.remove(&request.url);
						tokio::time::sleep(std::time::Duration::from_millis(crate::core::RETRY_DELAY_MS)).await;
						continue
					},
					Err(Error::DecodeExtrinsicError) => {
						error!("Decoding extrinsic failed");
						// Always retry for subxt errors (most of them are transient).
						let _ = connection_pool.remove(&request.url);
						tokio::time::sleep(std::time::Duration::from_millis(crate::core::RETRY_DELAY_MS)).await;
						continue
					},
				};

				// We only break in the happy case.
				let _ = request.response_sender.send(response);
				timeout_task.abort();
				break
			}
		} else {
			// channel closed, exit loop.
			panic!("Channel closed. Cascade failure ?")
		}
	}
}

async fn subxt_get_head(
	api: &polkadot::RuntimeApi<DefaultConfig, PolkadotExtrinsicParams<DefaultConfig>>,
	maybe_hash: Option<H256>,
) -> Result {
	Ok(Response::MaybeHead(api.client.rpc().header(maybe_hash).await.map_err(Error::SubxtError)?))
}

async fn subxt_get_block_ts(
	api: &polkadot::RuntimeApi<DefaultConfig, PolkadotExtrinsicParams<DefaultConfig>>,
	maybe_hash: Option<H256>,
) -> Result {
	Ok(Response::Timestamp(api.storage().timestamp().now(maybe_hash).await.map_err(Error::SubxtError)?))
}

async fn subxt_get_block(
	api: &polkadot::RuntimeApi<DefaultConfig, PolkadotExtrinsicParams<DefaultConfig>>,
	maybe_hash: Option<H256>,
) -> Result {
	Ok(Response::MaybeBlock(api.client.rpc().block(maybe_hash).await.map_err(Error::SubxtError)?))
}

/// Error originated from decoding an extrinsic.
#[derive(Clone, Debug)]
pub enum DecodeExtrinsicError {
	/// Expected more data.
	EarlyEof,
	/// Unsupported extrinsic.
	Unsupported,
	/// Failed to decode.
	CodecError(codec::Error),
}

/// Decode a Polkadot emitted extrinsic from provided bytes.
fn decode_extrinsic(data: &mut &[u8]) -> std::result::Result<SubxtCall, DecodeExtrinsicError> {
	// Extrinsic are encoded in memory in the following way:
	//   - Compact<u32>: Length of the extrinsic
	//   - first byte: abbbbbbb (a = 0 for unsigned, 1 for signed, b = version)
	//   - signature: emitted `ParaInherent` must be unsigned.
	//   - extrinsic data
	let _expected_length: Compact<u32> = Decode::decode(data).map_err(DecodeExtrinsicError::CodecError)?;
	if data.is_empty() {
		return Err(DecodeExtrinsicError::EarlyEof)
	}

	let is_signed = data[0] & 0b1000_0000 != 0;
	let version = data[0] & 0b0111_1111;
	*data = &data[1..];
	if is_signed || version != 4 {
		return Err(DecodeExtrinsicError::Unsupported)
	}

	SubxtCall::decode(data).map_err(DecodeExtrinsicError::CodecError)
}

fn subxt_extract_parainherent(block: &subxt::rpc::ChainBlock<DefaultConfig>) -> Result {
	// `ParaInherent` data is always at index #1.
	let bytes = block.block.extrinsics[1].encode();

	let data = match decode_extrinsic(&mut bytes.as_slice()).expect("Failed to decode `ParaInherent`") {
		SubxtCall::ParaInherent(
			subxt_runtime_types::polkadot_runtime_parachains::paras_inherent::pallet::Call::enter { data },
		) => data,
		_ => unimplemented!("Unhandled variant"),
	};
	Ok(Response::ParaInherentData(data))
}

#[derive(Debug)]
pub enum Error {
	SubxtError(subxt::BasicError),
	DecodeExtrinsicError,
}
pub type Result = std::result::Result<Response, Error>;
