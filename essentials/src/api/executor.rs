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

use crate::{
	api::{
		api_client::{build_light_client, build_online_client, ApiClientMode, ApiClientT, HeaderStream},
		dynamic::{
			decode_availability_cores, decode_claim_queue, decode_validator_groups, fetch_dynamic_storage,
			DynamicError, DynamicHostConfiguration,
		},
	},
	constants::MAX_MSG_QUEUE_SIZE,
	metadata::polkadot_primitives,
	types::{
		AccountId32, BlockNumber, ClaimQueue, CoreOccupied, Header, InherentData, SessionKeys, SubxtHrmpChannel,
		Timestamp, H256,
	},
	utils::{Retry, RetryOptions},
};
use log::error;
use polkadot_introspector_priority_channel::{
	channel, Receiver as PriorityReceiver, SendError as PrioritySendError, Sender as PrioritySender,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use subxt::PolkadotConfig;
use thiserror::Error;
use tokio::sync::oneshot::{error::RecvError as OneshotRecvError, Sender as OneshotSender};

enum ExecutorMessage {
	Close,
	Rpc(OneshotSender<Response>, Request),
}

#[derive(Debug, Clone)]
pub enum Request {
	GetBlockTimestamp(H256),
	GetHead(Option<H256>),
	GetBlockNumber(Option<H256>),
	GetBlockHash(Option<BlockNumber>),
	GetEvents(H256),
	ExtractParaInherent(Option<H256>),
	GetClaimQueue(H256),
	GetOccupiedCores(H256),
	GetBackingGroups(H256),
	GetSessionIndex(H256),
	GetSessionAccountKeys(u32),
	GetSessionNextKeys(AccountId32),
	GetInboundHRMPChannels(H256, u32),
	GetOutboundHRMPChannels(H256, u32),
	GetHostConfiguration,
	GetBestBlockSubscription,
	GetFinalizedBlockSubscription,
}

/// Response types for APIs.
#[derive(Debug)]
enum Response {
	/// A timestamp.
	Timestamp(Timestamp),
	/// A block header.
	MaybeHead(Option<Header>),
	/// A full block.
	BlockNumber(BlockNumber),
	/// A block hash.
	MaybeBlockHash(Option<H256>),
	/// Block events
	MaybeEvents(Option<subxt::events::Events<PolkadotConfig>>),
	/// `ParaInherent` data.
	ParaInherentData(InherentData),
	/// Claim queue for parachains.
	ClaimQueue(ClaimQueue),
	/// List of the occupied availability cores.
	OccupiedCores(Vec<CoreOccupied>),
	/// Backing validator groups.
	BackingGroups(Vec<Vec<polkadot_primitives::ValidatorIndex>>),
	/// Returns a session index
	SessionIndex(u32),
	/// Session keys
	SessionAccountKeys(Option<Vec<AccountId32>>),
	/// Session next keys for a validator
	SessionNextKeys(Option<SessionKeys>),
	/// HRMP channels for some parachain (e.g. who are sending messages to us)
	HRMPChannels(BTreeMap<u32, SubxtHrmpChannel>),
	/// The current host configuration
	HostConfiguration(DynamicHostConfiguration),
	/// Chain subscription
	ChainSubscription(HeaderStream),
}

#[derive(Debug, Error)]
pub enum RequestExecutorError {
	#[error("Client for url {0} not found")]
	ClientNotFound(String),
	#[error("subxt error: {0}")]
	SubxtError(#[from] subxt::error::Error),
	#[error("dynamic error: {0}")]
	DynamicError(#[from] DynamicError),
	#[error("subxt connection timeout")]
	Timeout,
	#[error("Send failed: {0}")]
	PrioritySendError(#[from] PrioritySendError),
	#[error("Recv failed: {0}")]
	OneshotRecvError(#[from] OneshotRecvError),
	#[error("Unexpected response for request {0:?}")]
	UnexpectedResponse(Request),
}

impl RequestExecutorError {
	pub fn should_repeat(&self) -> bool {
		matches!(
			self,
			RequestExecutorError::SubxtError(subxt::Error::Io(_)) |
				RequestExecutorError::SubxtError(subxt::Error::Rpc(_))
		)
	}
}

struct RequestExecutorBackend {
	retry: RetryOptions,
}

impl RequestExecutorBackend {
	async fn run(
		&mut self,
		from_frontend: PriorityReceiver<ExecutorMessage>,
		url: String,
		api_client_mode: ApiClientMode,
	) {
		let client = match build_client(&url, api_client_mode, &self.retry).await {
			Some(v) => v,
			None => return,
		};

		loop {
			tokio::select! {
				message = from_frontend.recv() => {
					match message {
						Ok(message) => match message {
							ExecutorMessage::Close => return,
							ExecutorMessage::Rpc(tx, request) => {
								match self.execute_request(&request, &*client).await {
									Ok(response) => {
										// Not critical, skip it and process next request
										if let Err(e) = tx.send(response) {
											error!("Cannot send response back: {:?}", e);
										}
									},
									// Critical, after a few retries RPC client was not able to process the request
									Err(e) => return error!("Cannot process the request {:?}: {:?}", request, e),
								};
							}
						},
						// Not critical, skip it and process next request
						Err(e) => error!("Cannot receive a request from the frontend: {:?}", e),
					}
				}
			}
		}
	}

	async fn execute_request(
		&mut self,
		request: &Request,
		client: &dyn ApiClientT,
	) -> color_eyre::Result<Response, RequestExecutorError> {
		let mut retry = Retry::new(&self.retry);
		loop {
			match self.match_request(request.to_owned(), client).await {
				Ok(v) => return Ok(v),
				Err(e) => {
					if !e.should_repeat() {
						return Err(e)
					}
					if (retry.sleep().await).is_err() {
						return Err(RequestExecutorError::Timeout)
					}
				},
			}
		}
	}

	async fn match_request(
		&mut self,
		request: Request,
		client: &dyn ApiClientT,
	) -> color_eyre::Result<Response, RequestExecutorError> {
		use Request::*;
		use Response::*;
		let response = match request {
			GetBlockTimestamp(hash) => Timestamp(client.get_block_ts(hash).await?.unwrap_or_default()),
			GetHead(maybe_hash) => {
				let head = match client.get_head(maybe_hash).await {
					Ok(v) => Some(v),
					Err(subxt::Error::Block(subxt::error::BlockError::NotFound(_))) => None,
					Err(err) => return Err(err.into()),
				};
				MaybeHead(head)
			},
			GetBlockNumber(maybe_hash) => BlockNumber(client.get_block_number(maybe_hash).await?),
			GetBlockHash(maybe_block_number) => MaybeBlockHash(client.legacy_get_block_hash(maybe_block_number).await?),
			GetEvents(hash) => MaybeEvents(Some(client.get_events(hash).await?)),
			ExtractParaInherent(maybe_hash) => ParaInherentData(client.extract_parainherent(maybe_hash).await?),
			GetClaimQueue(hash) => {
				let value = fetch_dynamic_storage(client, Some(hash), "ParaScheduler", "ClaimQueue").await?;
				ClaimQueue(decode_claim_queue(&value)?)
			},
			GetOccupiedCores(hash) => {
				let value = fetch_dynamic_storage(client, Some(hash), "ParaScheduler", "AvailabilityCores").await?;
				OccupiedCores(decode_availability_cores(&value)?)
			},
			GetBackingGroups(hash) => {
				let value = fetch_dynamic_storage(client, Some(hash), "ParaScheduler", "ValidatorGroups").await?;
				BackingGroups(decode_validator_groups(&value)?)
			},
			GetSessionIndex(hash) => SessionIndex(client.get_session_index(hash).await?.unwrap_or_default()),
			GetSessionAccountKeys(session_index) =>
				SessionAccountKeys(client.get_session_account_keys(session_index).await?),
			GetSessionNextKeys(ref account) => SessionNextKeys(client.get_session_next_keys(account).await?),
			GetInboundHRMPChannels(hash, para_id) =>
				HRMPChannels(client.get_inbound_hrmp_channels(hash, para_id).await?),
			GetOutboundHRMPChannels(hash, para_id) =>
				HRMPChannels(client.get_outbound_hrmp_channels(hash, para_id).await?),
			GetHostConfiguration => HostConfiguration(DynamicHostConfiguration::new(
				fetch_dynamic_storage(client, None, "Configuration", "ActiveConfig").await?,
			)),
			GetBestBlockSubscription => ChainSubscription(client.stream_best_block_headers().await?),
			GetFinalizedBlockSubscription => ChainSubscription(client.stream_finalized_block_headers().await?),
		};

		Ok(response)
	}
}

pub trait RequestExecutorNodes {
	fn unique_nodes(&self) -> impl Iterator<Item = String>;
}

impl RequestExecutorNodes for Vec<String> {
	fn unique_nodes(&self) -> impl Iterator<Item = String> {
		self.iter().collect::<HashSet<_>>().into_iter().cloned()
	}
}

impl RequestExecutorNodes for String {
	fn unique_nodes(&self) -> impl Iterator<Item = String> {
		std::iter::once(self).into_iter().cloned()
	}
}

impl RequestExecutorNodes for &str {
	fn unique_nodes(&self) -> impl Iterator<Item = String> {
		std::iter::once(self.to_string()).into_iter()
	}
}

#[derive(Clone)]
pub struct RequestExecutor(HashMap<String, PrioritySender<ExecutorMessage>>);

macro_rules! wrap_backend_call {
	($self:expr, $url:expr, $request_ty:ident, $response_ty:ident) => {
		if let Some(to_backend) = $self.0.get_mut($url) {
			let (tx, rx) = tokio::sync::oneshot::channel::<Response>();
			let request = Request::$request_ty;
			to_backend.send(ExecutorMessage::Rpc(tx, Request::$request_ty)).await?;
			match rx.await? {
				Response::$response_ty(res) => Ok(res),
				response => {
					error!("Unexpected request-response pair: {:?} {:?}", request, response);
					Err(RequestExecutorError::UnexpectedResponse(request))
				},
			}
		} else {
			Err(RequestExecutorError::ClientNotFound($url.into()))
		}
	};
	($self:expr, $url:expr, $request_ty:ident, $response_ty:ident, $($arg:expr),*) => {
		if let Some(to_backend) = $self.0.get_mut($url) {
			let (tx, rx) = tokio::sync::oneshot::channel::<Response>();
			let request = Request::$request_ty($($arg),*);
			to_backend.send(ExecutorMessage::Rpc(tx, request.clone())).await?;
			match rx.await? {
				Response::$response_ty(res) => Ok(res),
				response => {
					error!("Unexpected request-response pair: {:?} {:?}", request, response);
					Err(RequestExecutorError::UnexpectedResponse(request))
				},
			}
		} else {
			Err(RequestExecutorError::ClientNotFound($url.into()))
		}
	};
}

impl RequestExecutor {
	/// Creates new RPC executor
	pub fn build(
		nodes: impl RequestExecutorNodes,
		api_client_mode: ApiClientMode,
		retry: RetryOptions,
	) -> color_eyre::Result<RequestExecutor, RequestExecutorError> {
		let mut clients = HashMap::new();
		for node in nodes.unique_nodes() {
			let (to_backend, from_frontend) = channel(MAX_MSG_QUEUE_SIZE);
			let _ = clients.insert(node.clone(), to_backend);
			let retry = retry.clone();
			let api_client_mode = api_client_mode;
			tokio::spawn(async move {
				let mut backend = RequestExecutorBackend { retry };
				backend.run(from_frontend, node, api_client_mode).await;
			});
		}

		Ok(RequestExecutor(clients))
	}

	/// Closes all RPC clients
	pub async fn close(&mut self) -> color_eyre::Result<()> {
		for to_backend in self.0.values_mut() {
			to_backend.send(ExecutorMessage::Close).await?;
		}

		Ok(())
	}

	pub async fn get_block_timestamp(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Timestamp, RequestExecutorError> {
		wrap_backend_call!(self, url, GetBlockTimestamp, Timestamp, hash)
	}

	pub async fn get_block_head(
		&mut self,
		url: &str,
		maybe_hash: Option<H256>,
	) -> color_eyre::Result<Option<Header>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetHead, MaybeHead, maybe_hash)
	}

	pub async fn get_block_number(
		&mut self,
		url: &str,
		maybe_hash: Option<H256>,
	) -> color_eyre::Result<BlockNumber, RequestExecutorError> {
		wrap_backend_call!(self, url, GetBlockNumber, BlockNumber, maybe_hash)
	}

	pub async fn get_block_hash(
		&mut self,
		url: &str,
		maybe_block_number: Option<BlockNumber>,
	) -> color_eyre::Result<Option<H256>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetBlockHash, MaybeBlockHash, maybe_block_number)
	}

	pub async fn get_events(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Option<subxt::events::Events<PolkadotConfig>>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetEvents, MaybeEvents, hash)
	}

	pub async fn extract_parainherent_data(
		&mut self,
		url: &str,
		maybe_hash: Option<H256>,
	) -> color_eyre::Result<InherentData, RequestExecutorError> {
		wrap_backend_call!(self, url, ExtractParaInherent, ParaInherentData, maybe_hash)
	}

	pub async fn get_claim_queue(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<ClaimQueue, RequestExecutorError> {
		wrap_backend_call!(self, url, GetClaimQueue, ClaimQueue, hash)
	}

	pub async fn get_occupied_cores(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Vec<CoreOccupied>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetOccupiedCores, OccupiedCores, hash)
	}

	pub async fn get_backing_groups(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Vec<Vec<polkadot_primitives::ValidatorIndex>>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetBackingGroups, BackingGroups, hash)
	}

	pub async fn get_session_index(&mut self, url: &str, hash: H256) -> color_eyre::Result<u32, RequestExecutorError> {
		wrap_backend_call!(self, url, GetSessionIndex, SessionIndex, hash)
	}

	pub async fn get_session_account_keys(
		&mut self,
		url: &str,
		session_index: u32,
	) -> color_eyre::Result<Option<Vec<AccountId32>>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetSessionAccountKeys, SessionAccountKeys, session_index)
	}

	pub async fn get_session_next_keys(
		&mut self,
		url: &str,
		account: AccountId32,
	) -> color_eyre::Result<Option<SessionKeys>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetSessionNextKeys, SessionNextKeys, account)
	}

	pub async fn get_inbound_hrmp_channels(
		&mut self,
		url: &str,
		hash: H256,
		para_id: u32,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetInboundHRMPChannels, HRMPChannels, hash, para_id)
	}

	pub async fn get_outbound_hrmp_channels(
		&mut self,
		url: &str,
		hash: H256,
		para_id: u32,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetOutboundHRMPChannels, HRMPChannels, hash, para_id)
	}

	pub async fn get_host_configuration(
		&mut self,
		url: &str,
	) -> color_eyre::Result<DynamicHostConfiguration, RequestExecutorError> {
		wrap_backend_call!(self, url, GetHostConfiguration, HostConfiguration)
	}

	pub async fn get_best_block_subscription(
		&mut self,
		url: &str,
	) -> color_eyre::Result<HeaderStream, RequestExecutorError> {
		wrap_backend_call!(self, url, GetBestBlockSubscription, ChainSubscription)
	}

	pub async fn get_finalized_block_subscription(
		&mut self,
		url: &str,
	) -> color_eyre::Result<HeaderStream, RequestExecutorError> {
		wrap_backend_call!(self, url, GetFinalizedBlockSubscription, ChainSubscription)
	}
}

// Attempts to connect to websocket and returns an RuntimeApi instance if successful.
async fn build_client(url: &str, api_client_mode: ApiClientMode, retry: &RetryOptions) -> Option<Box<dyn ApiClientT>> {
	let mut retry = Retry::new(retry);
	loop {
		match api_client_mode {
			ApiClientMode::RPC => match build_online_client(url).await {
				Ok(client) => return Some(Box::new(client)),
				Err(err) => {
					error!("[{}] RpcClient error: {:?}", url, err);
					if (retry.sleep().await).is_err() {
						return None
					}
				},
			},
			ApiClientMode::Light => match build_light_client(url).await {
				Ok(client) => return Some(Box::new(client)),
				Err(err) => {
					error!("[{}] LightClient error: {:?}", url, err);
					if (retry.sleep().await).is_err() {
						return None
					}
				},
			},
		}
	}
}
