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
		api_client::{ApiClient, ApiClientMode, HeaderStream, build_online_client},
		dynamic::{self, DynamicHostConfiguration, decode_validator_groups, fetch_dynamic_storage},
	},
	constants::MAX_MSG_QUEUE_SIZE,
	init::Shutdown,
	metadata::{
		polkadot::session::storage::types::queued_keys::QueuedKeys, polkadot_primitives,
		polkadot_staging_primitives::CoreState,
	},
	types::{
		AccountId32, BlockNumber, ClaimQueue, CoreOccupied, H256, Header, InboundOutBoundHrmpChannels, InherentData,
		PolkadotHasher, SessionKeys, SubxtHrmpChannel, Timestamp,
	},
	utils::{Retry, RetryOptions},
};
use color_eyre::eyre::eyre;
use log::error;
use polkadot_introspector_priority_channel::{
	Receiver as PriorityReceiver, SendError as PrioritySendError, Sender as PrioritySender, channel,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use subxt::{OnlineClient, PolkadotConfig};
use thiserror::Error;
use tokio::sync::{
	broadcast::Sender as BroadcastSender,
	oneshot::{Sender as OneshotSender, error::RecvError as OneshotRecvError},
};

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
	GetSessionAccountKeys(u32, Option<H256>),
	GetSessionNextKeys(AccountId32),
	GetSessionQueuedKeys(Option<H256>),
	GetInboundOutBoundHrmpChannels(H256, Vec<u32>),
	GetSessionIndexNow,
	GetHostConfiguration,
	GetBestBlockSubscription,
	GetFinalizedBlockSubscription,
	GetChainName,
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
	/// Session queued keys.
	SessionQueuedKeys(Option<QueuedKeys>),
	/// HRMP channels for given parachain (e.g. who are sending messages to us)
	InboundOutBoundHrmpChannels(InboundOutBoundHrmpChannels),
	/// The current host configuration
	HostConfiguration(DynamicHostConfiguration),
	/// Chain subscription
	ChainSubscription(HeaderStream),
	/// Chain name
	ChainName(String),
}

#[derive(Debug, Error)]
pub enum RequestExecutorError {
	#[error("Cannot build a RPC client for {0}")]
	ClientBuildFailed(String),
	#[error("Client for url {0} not found")]
	ClientNotFound(String),
	#[error("subxt error: {0}")]
	SubxtError(String),
	#[error("subxt retriable error: {0}")]
	SubxtRetriableError(String),
	#[error("dynamic error: {0}")]
	DynamicError(#[from] dynamic::DynamicError),
	#[error("subxt connection timeout")]
	Timeout,
	#[error("Send failed: {0}")]
	PrioritySendError(#[from] PrioritySendError),
	#[error("Recv failed: {0}")]
	OneshotRecvError(#[from] OneshotRecvError),
	#[error("Unexpected response for request {0:?}")]
	UnexpectedResponse(Request),
}

impl From<subxt::error::Error> for RequestExecutorError {
	fn from(err: subxt::error::Error) -> Self {
		match err {
			subxt::Error::Io(_) | subxt::Error::Rpc(_) => Self::SubxtRetriableError(err.to_string()),
			_ => Self::SubxtError(err.to_string()),
		}
	}
}

impl RequestExecutorError {
	pub fn should_retry(&self) -> bool {
		matches!(self, Self::SubxtRetriableError(_))
	}
}

struct RequestExecutorBackend {
	client: ApiClient<OnlineClient<PolkadotConfig>>,
	retry: RetryOptions,
}

impl RequestExecutorBackend {
	async fn build(
		retry: RetryOptions,
		url: String,
		api_client_mode: ApiClientMode,
	) -> color_eyre::Result<Self, RequestExecutorError> {
		let client = build_client(&url, api_client_mode, &retry)
			.await
			.ok_or(RequestExecutorError::ClientBuildFailed(url))?;

		Ok(RequestExecutorBackend { client, retry })
	}

	async fn run(&mut self, from_frontend: PriorityReceiver<ExecutorMessage>) -> color_eyre::Result<()> {
		loop {
			match from_frontend.recv().await? {
				ExecutorMessage::Close => return Ok(()),
				ExecutorMessage::Rpc(tx, request) => {
					match self.execute_request(&request).await {
						Ok(response) => {
							// Not critical, skip it and process next request
							if let Err(e) = tx.send(response) {
								error!("Cannot send response back: {:?}", e);
							}
						},
						// Critical, after a few retries RPC client was not able to process the request
						Err(e) => return Err(eyre!("Cannot process the request {:?}: {:?}", request, e)),
					};
				},
			}
		}
	}

	async fn execute_request(&mut self, request: &Request) -> color_eyre::Result<Response, RequestExecutorError> {
		let mut retry = Retry::new(&self.retry);
		loop {
			match self.match_request(request.to_owned()).await {
				Ok(v) => return Ok(v),
				Err(e) => {
					if !e.should_retry() {
						return Err(e)
					}
					if (retry.sleep().await).is_err() {
						return Err(RequestExecutorError::Timeout)
					}
				},
			}
		}
	}

	async fn match_request(&self, request: Request) -> color_eyre::Result<Response, RequestExecutorError> {
		use Request::*;
		use Response::*;
		let client = &self.client;
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
			GetChainName => ChainName(client.legacy_get_chain_name().await?),
			GetEvents(hash) => MaybeEvents(Some(client.get_events(hash).await?)),
			ExtractParaInherent(maybe_hash) => ParaInherentData(client.extract_parainherent(maybe_hash).await?),
			GetClaimQueue(hash) => ClaimQueue(client.get_claim_queue(hash).await?),
			GetOccupiedCores(hash) => {
				let value = client
					.get_occupied_cores(hash)
					.await?
					.iter()
					.map(|core_state| match core_state {
						CoreState::Free => CoreOccupied::Free,
						CoreState::Scheduled(_) => CoreOccupied::Scheduled,
						CoreState::Occupied(_) => CoreOccupied::Occupied,
					})
					.collect();
				OccupiedCores(value)
			},
			GetBackingGroups(hash) => {
				let value = fetch_dynamic_storage(client, Some(hash), "ParaScheduler", "ValidatorGroups").await?;
				BackingGroups(decode_validator_groups(&value)?)
			},
			GetSessionIndex(hash) => SessionIndex(client.get_session_index(hash).await?.unwrap_or_default()),
			GetSessionAccountKeys(session_index, maybe_hash) =>
				SessionAccountKeys(client.get_session_account_keys(session_index, maybe_hash).await?),
			GetSessionNextKeys(ref account) => SessionNextKeys(client.get_session_next_keys(account).await?),
			GetSessionQueuedKeys(at) => SessionQueuedKeys(client.get_session_queued_keys(at).await?),
			GetSessionIndexNow => SessionIndex(client.get_session_index_now().await?.unwrap_or_default()),
			GetInboundOutBoundHrmpChannels(hash, para_ids) =>
				InboundOutBoundHrmpChannels(client.get_inbound_outbound_hrmp_channels(hash, para_ids).await?),
			GetHostConfiguration => HostConfiguration(DynamicHostConfiguration::new(
				fetch_dynamic_storage(client, None, "Configuration", "ActiveConfig").await?,
			)),
			GetBestBlockSubscription => ChainSubscription(client.stream_best_block_headers().await?),
			GetFinalizedBlockSubscription => ChainSubscription(client.stream_finalized_block_headers().await?),
		};

		Ok(response)
	}

	pub fn hasher(&self) -> PolkadotHasher {
		self.client.hasher()
	}
}

pub trait RequestExecutorNodes {
	fn unique_nodes(&self) -> HashSet<String>;
}

impl RequestExecutorNodes for Vec<String> {
	fn unique_nodes(&self) -> HashSet<String> {
		self.iter().cloned().collect()
	}
}

impl RequestExecutorNodes for String {
	fn unique_nodes(&self) -> HashSet<String> {
		std::iter::once(self).cloned().collect()
	}
}

impl RequestExecutorNodes for &str {
	fn unique_nodes(&self) -> HashSet<String> {
		std::iter::once(self.to_string()).collect()
	}
}

#[derive(Clone)]
pub struct RequestExecutor(HashMap<String, (PrioritySender<ExecutorMessage>, PolkadotHasher)>);

macro_rules! wrap_backend_call {
	($self:expr, $url:expr, $request_ty:ident, $response_ty:ident) => {
		if let Some((to_backend, _)) = $self.0.get_mut($url) {
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
		if let Some((to_backend, _)) = $self.0.get_mut($url) {
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
	pub async fn build(
		nodes: impl RequestExecutorNodes,
		api_client_mode: ApiClientMode,
		retry: &RetryOptions,
		shutdown_tx: &BroadcastSender<Shutdown>,
	) -> color_eyre::Result<RequestExecutor, RequestExecutorError> {
		let mut clients = HashMap::new();
		for node in nodes.unique_nodes() {
			let (to_backend, from_frontend) = channel(MAX_MSG_QUEUE_SIZE);
			let mut backend = RequestExecutorBackend::build(retry.clone(), node.clone(), api_client_mode).await?;
			let _ = clients.insert(node, (to_backend, backend.hasher()));
			let shutdown_tx = shutdown_tx.clone();
			tokio::spawn(async move {
				if let Err(e) = backend.run(from_frontend).await {
					error!("Request Executor Backend failed to run, closing the application: {:?}", e);
					let _ = shutdown_tx.send(Shutdown::Restart);
				}
			});
		}

		Ok(RequestExecutor(clients))
	}

	pub fn hasher(&self, url: &str) -> Option<PolkadotHasher> {
		self.0.get(url).map(|(_, hasher)| *hasher)
	}

	/// Closes all RPC clients
	pub async fn close(&mut self) {
		for (to_backend, _) in self.0.values_mut() {
			let _ = to_backend.send(ExecutorMessage::Close).await;
		}
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

	pub async fn get_chain_name(&mut self, url: &str) -> color_eyre::Result<String, RequestExecutorError> {
		wrap_backend_call!(self, url, GetChainName, ChainName)
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

	pub async fn get_session_index_now(&mut self, url: &str) -> color_eyre::Result<u32, RequestExecutorError> {
		wrap_backend_call!(self, url, GetSessionIndexNow, SessionIndex)
	}

	pub async fn get_session_account_keys(
		&mut self,
		url: &str,
		session_index: u32,
		maybe_hash: Option<H256>,
	) -> color_eyre::Result<Option<Vec<AccountId32>>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetSessionAccountKeys, SessionAccountKeys, session_index, maybe_hash)
	}

	pub async fn get_session_next_keys(
		&mut self,
		url: &str,
		account: AccountId32,
	) -> color_eyre::Result<Option<SessionKeys>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetSessionNextKeys, SessionNextKeys, account)
	}

	pub async fn get_session_queued_keys(
		&mut self,
		url: &str,
		at: Option<H256>,
	) -> color_eyre::Result<Option<QueuedKeys>, RequestExecutorError> {
		wrap_backend_call!(self, url, GetSessionQueuedKeys, SessionQueuedKeys, at)
	}

	pub async fn get_inbound_outbound_hrmp_channels(
		&mut self,
		url: &str,
		hash: H256,
		para_ids: Vec<u32>,
	) -> color_eyre::Result<
		Vec<(u32, BTreeMap<u32, SubxtHrmpChannel>, BTreeMap<u32, SubxtHrmpChannel>)>,
		RequestExecutorError,
	> {
		wrap_backend_call!(self, url, GetInboundOutBoundHrmpChannels, InboundOutBoundHrmpChannels, hash, para_ids)
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
async fn build_client(
	url: &str,
	api_client_mode: ApiClientMode,
	retry: &RetryOptions,
) -> Option<ApiClient<OnlineClient<PolkadotConfig>>> {
	let mut retry = Retry::new(retry);
	loop {
		match build_online_client(url, api_client_mode).await {
			Ok(client) => return Some(client),
			Err(err) => {
				error!("[{}] RpcClient error: {:?}", url, err);
				if (retry.sleep().await).is_err() {
					return None
				}
			},
		}
	}
}
