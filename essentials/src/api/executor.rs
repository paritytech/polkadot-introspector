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
use std::collections::{hash_map::Entry, BTreeMap, HashMap};
use subxt::PolkadotConfig;
use thiserror::Error;
use tokio::sync::oneshot::{error::RecvError as OneshotRecvError, Sender as OneshotSender};

enum ExecutorMessage {
	Close,
	Rpc(OneshotSender<RpcResponse>, RpcRequest),
}

#[derive(Debug, Clone)]
pub enum RpcRequest {
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

impl std::fmt::Display for RpcRequest {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		use RpcRequest::*;
		match *self {
			GetBlockTimestamp(hash) => write!(f, "GetBlockTimestamp({})", hash),
			GetHead(maybe_hash) => write!(f, "GetHead({:?})", maybe_hash),
			GetBlockNumber(maybe_hash) => write!(f, "GetBlockNumber({:?})", maybe_hash),
			GetBlockHash(maybe_block_number) => write!(f, "GetBlockHash({:?})", maybe_block_number),
			GetEvents(hash) => write!(f, "GetEvents({})", hash),
			ExtractParaInherent(maybe_hash) => write!(f, "ExtractParaInherent({:?})", maybe_hash),
			GetClaimQueue(hash) => write!(f, "GetClaimQueue({})", hash),
			GetOccupiedCores(hash) => write!(f, "GetOccupiedCores({})", hash),
			GetBackingGroups(hash) => write!(f, "GetBackingGroups({})", hash),
			GetSessionIndex(hash) => write!(f, "GetSessionIndex({})", hash),
			GetSessionAccountKeys(session) => write!(f, "GetSessionAccountKeys({})", session),
			GetSessionNextKeys(ref account) => write!(f, "GetSessionNextKeys({})", account),
			GetInboundHRMPChannels(hash, para_id) => write!(f, "GetInboundHRMPChannels({}, {})", hash, para_id),
			GetOutboundHRMPChannels(hash, para_id) => write!(f, "GetOutboundHRMPChannels({}, {})", hash, para_id),
			GetHostConfiguration => write!(f, "GetHostConfiguration"),
			GetBestBlockSubscription => write!(f, "GetBestBlockSubscription"),
			GetFinalizedBlockSubscription => write!(f, "GetFinalizedBlockSubscription"),
		}
	}
}

/// Response types for APIs.
#[derive(Debug)]
enum RpcResponse {
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
pub enum RpcExecutorError {
	#[error("Client for url {0} already exists")]
	ClientAlreadyExists(String),
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
	#[error("Unexpected response for request {0}")]
	UnexpectedResponse(RpcRequest),
}

struct BackendExecutor {
	retry: RetryOptions,
}

impl BackendExecutor {
	async fn start(
		&mut self,
		from_frontend: PriorityReceiver<ExecutorMessage>,
		url: String,
		api_client_mode: ApiClientMode,
	) {
		let client = match new_client_fn(&url, api_client_mode, &self.retry).await {
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
									Ok(v) => {
										tx.send(v).unwrap();
									},
									Err(e) => println!("BackendExecutor: {:?}", e),
								};
							}
						},
						Err(e) => println!("BackendExecutor: {:?}", e),
					}
				}
			}
		}
	}

	async fn execute_request(
		&mut self,
		request: &RpcRequest,
		client: &dyn ApiClientT,
	) -> color_eyre::Result<RpcResponse, RpcExecutorError> {
		let mut retry = Retry::new(&self.retry);
		loop {
			match self.match_request(request.to_owned(), client).await {
				Ok(v) => return Ok(v),
				Err(e) => {
					if !matches!(
						e,
						RpcExecutorError::SubxtError(subxt::Error::Io(_)) |
							RpcExecutorError::SubxtError(subxt::Error::Rpc(_))
					) {
						return Err(e)
					}

					if (retry.sleep().await).is_err() {
						return Err(RpcExecutorError::Timeout)
					}
				},
			}
		}
	}

	async fn match_request(
		&mut self,
		request: RpcRequest,
		client: &dyn ApiClientT,
	) -> color_eyre::Result<RpcResponse, RpcExecutorError> {
		use RpcRequest::*;
		use RpcResponse::*;
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

macro_rules! wrap_backend_call {
	($self:expr, $url:expr, $request_ty:ident, $response_ty:ident) => {
		if let Some(to_backend) = $self.connection_pool.get_mut($url) {
			let (tx, rx) = tokio::sync::oneshot::channel::<RpcResponse>();
			to_backend.send(ExecutorMessage::Rpc(tx, RpcRequest::$request_ty)).await?;
			match rx.await? {
				RpcResponse::$response_ty(res) => Ok(res),
				_ => Err(RpcExecutorError::UnexpectedResponse(RpcRequest::$request_ty)),
			}
		} else {
			Err(RpcExecutorError::ClientNotFound($url.into()))
		}
	};
	($self:expr, $url:expr, $request_ty:ident, $response_ty:ident, $($arg:expr),*) => {
		if let Some(to_backend) = $self.connection_pool.get_mut($url) {
			let (tx, rx) = tokio::sync::oneshot::channel::<RpcResponse>();
			let request = RpcRequest::$request_ty($($arg),*);
			to_backend.send(ExecutorMessage::Rpc(tx, request.clone())).await?;
			match rx.await? {
				RpcResponse::$response_ty(res) => Ok(res),
				_ => Err(RpcExecutorError::UnexpectedResponse(request)),
			}
		} else {
			Err(RpcExecutorError::ClientNotFound($url.into()))
		}
	};
}

#[derive(Clone)]
pub struct RpcExecutor {
	connection_pool: HashMap<String, PrioritySender<ExecutorMessage>>,
	api_client_mode: ApiClientMode,
	retry: RetryOptions,
}

impl RpcExecutor {
	/// Creates new RPC executor
	pub fn new(api_client_mode: ApiClientMode, retry: RetryOptions) -> Self {
		Self { api_client_mode, retry, connection_pool: Default::default() }
	}

	/// Starts new RPC client
	pub fn start(&mut self, url: String) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>, RpcExecutorError> {
		match self.connection_pool.entry(url.clone()) {
			Entry::Occupied(_) => Err(RpcExecutorError::ClientAlreadyExists(url)),
			Entry::Vacant(entry) => {
				let (to_backend, from_frontend) = channel(MAX_MSG_QUEUE_SIZE);
				let _ = entry.insert(to_backend);
				let retry = self.retry.clone();
				let api_client_mode = self.api_client_mode;
				let fut = async move {
					let mut backend = BackendExecutor { retry };
					backend.start(from_frontend, url, api_client_mode).await;
				};

				Ok(vec![tokio::spawn(fut)])
			},
		}
	}

	/// Closes all RPC clients
	pub async fn close(&mut self) -> color_eyre::Result<()> {
		for to_backend in self.connection_pool.values_mut() {
			to_backend.send(ExecutorMessage::Close).await?;
		}

		Ok(())
	}

	pub async fn get_block_timestamp(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Timestamp, RpcExecutorError> {
		wrap_backend_call!(self, url, GetBlockTimestamp, Timestamp, hash)
	}

	pub async fn get_block_head(
		&mut self,
		url: &str,
		maybe_hash: Option<H256>,
	) -> color_eyre::Result<Option<Header>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetHead, MaybeHead, maybe_hash)
	}

	pub async fn get_block_number(
		&mut self,
		url: &str,
		maybe_hash: Option<H256>,
	) -> color_eyre::Result<BlockNumber, RpcExecutorError> {
		wrap_backend_call!(self, url, GetBlockNumber, BlockNumber, maybe_hash)
	}

	pub async fn get_block_hash(
		&mut self,
		url: &str,
		maybe_block_number: Option<BlockNumber>,
	) -> color_eyre::Result<Option<H256>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetBlockHash, MaybeBlockHash, maybe_block_number)
	}

	pub async fn get_events(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Option<subxt::events::Events<PolkadotConfig>>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetEvents, MaybeEvents, hash)
	}

	pub async fn extract_parainherent_data(
		&mut self,
		url: &str,
		maybe_hash: Option<H256>,
	) -> color_eyre::Result<InherentData, RpcExecutorError> {
		wrap_backend_call!(self, url, ExtractParaInherent, ParaInherentData, maybe_hash)
	}

	pub async fn get_claim_queue(&mut self, url: &str, hash: H256) -> color_eyre::Result<ClaimQueue, RpcExecutorError> {
		wrap_backend_call!(self, url, GetClaimQueue, ClaimQueue, hash)
	}

	pub async fn get_occupied_cores(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Vec<CoreOccupied>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetOccupiedCores, OccupiedCores, hash)
	}

	pub async fn get_backing_groups(
		&mut self,
		url: &str,
		hash: H256,
	) -> color_eyre::Result<Vec<Vec<polkadot_primitives::ValidatorIndex>>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetBackingGroups, BackingGroups, hash)
	}

	pub async fn get_session_index(&mut self, url: &str, hash: H256) -> color_eyre::Result<u32, RpcExecutorError> {
		wrap_backend_call!(self, url, GetSessionIndex, SessionIndex, hash)
	}

	pub async fn get_session_account_keys(
		&mut self,
		url: &str,
		session_index: u32,
	) -> color_eyre::Result<Option<Vec<AccountId32>>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetSessionAccountKeys, SessionAccountKeys, session_index)
	}

	pub async fn get_session_next_keys(
		&mut self,
		url: &str,
		account: AccountId32,
	) -> color_eyre::Result<Option<SessionKeys>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetSessionNextKeys, SessionNextKeys, account)
	}

	pub async fn get_inbound_hrmp_channels(
		&mut self,
		url: &str,
		hash: H256,
		para_id: u32,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetInboundHRMPChannels, HRMPChannels, hash, para_id)
	}

	pub async fn get_outbound_hrmp_channels(
		&mut self,
		url: &str,
		hash: H256,
		para_id: u32,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, RpcExecutorError> {
		wrap_backend_call!(self, url, GetOutboundHRMPChannels, HRMPChannels, hash, para_id)
	}

	pub async fn get_host_configuration(
		&mut self,
		url: &str,
	) -> color_eyre::Result<DynamicHostConfiguration, RpcExecutorError> {
		wrap_backend_call!(self, url, GetHostConfiguration, HostConfiguration)
	}

	pub async fn get_best_block_subscription(
		&mut self,
		url: &str,
	) -> color_eyre::Result<HeaderStream, RpcExecutorError> {
		wrap_backend_call!(self, url, GetBestBlockSubscription, ChainSubscription)
	}

	pub async fn get_finalized_block_subscription(
		&mut self,
		url: &str,
	) -> color_eyre::Result<HeaderStream, RpcExecutorError> {
		wrap_backend_call!(self, url, GetFinalizedBlockSubscription, ChainSubscription)
	}
}

// Attempts to connect to websocket and returns an RuntimeApi instance if successful.
async fn new_client_fn(url: &str, api_client_mode: ApiClientMode, retry: &RetryOptions) -> Option<Box<dyn ApiClientT>> {
	let mut retry = Retry::new(retry);

	loop {
		match api_client_mode {
			ApiClientMode::RPC => match build_online_client(url).await {
				Ok(client) => return Some(Box::new(client)),
				Err(err) => {
					error!("[{}] Client error: {:?}", url, err);
					if (retry.sleep().await).is_err() {
						return None
					}
				},
			},
			ApiClientMode::Light => match build_light_client(url).await {
				Ok(client) => return Some(Box::new(client)),
				Err(err) => {
					error!("[{}] Client error: {:?}", url, err);
					if (retry.sleep().await).is_err() {
						return None
					}
				},
			},
		}
	}
}
