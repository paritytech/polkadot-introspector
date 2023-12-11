use super::{
	api_client::{build_light_client, build_online_client, HeaderStream},
	subxt_wrapper::ApiClientMode,
};
use crate::{
	api::api_client::ApiClientT,
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
use subxt::{
	dynamic::{At, Value},
	ext::scale_value::ValueDef,
	PolkadotConfig,
};
use thiserror::Error;
use tokio::sync::oneshot::{error::RecvError as OneshotRecvError, Sender as OneshotSender};

enum ExecutorMessage {
	Close,
	Rpc(OneshotSender<RpcResponse>, RpcRequest),
}

#[derive(Debug, Clone)]
enum RpcRequest {
	GetBlockTimestamp(H256),
	GetHead,
	GetBlockNumber,
	GetBlockHash,
	GetEvents,
	ExtractParaInherent,
	GetClaimQueue,
	GetOccupiedCores,
	GetBackingGroups,
	GetSessionIndex,
	GetSessionAccountKeys,
	GetSessionNextKeys,
	GetInboundHRMPChannels,
	GetOutboundHRMPChannels,
	GetHostConfiguration,
	GetBestBlockSubscription,
	GetFinalizedBlockSubscription,
}

impl std::fmt::Display for RpcRequest {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		use RpcRequest::*;
		match *self {
			GetBlockTimestamp(hash) => write!(f, "GetBlockTimestamp({})", hash),
			GetHead => write!(f, "GetHead"),
			GetBlockNumber => write!(f, "GetBlockNumber"),
			GetBlockHash => write!(f, "GetBlockHash"),
			GetEvents => write!(f, "GetEvents"),
			ExtractParaInherent => write!(f, "ExtractParaInherent"),
			GetClaimQueue => write!(f, "GetClaimQueue"),
			GetOccupiedCores => write!(f, "GetOccupiedCores"),
			GetBackingGroups => write!(f, "GetBackingGroups"),
			GetSessionIndex => write!(f, "GetSessionIndex"),
			GetSessionAccountKeys => write!(f, "GetSessionAccountKeys"),
			GetSessionNextKeys => write!(f, "GetSessionNextKeys"),
			GetInboundHRMPChannels => write!(f, "GetInboundHRMPChannels"),
			GetOutboundHRMPChannels => write!(f, "GetOutboundHRMPChannels"),
			GetHostConfiguration => write!(f, "GetHostConfiguration"),
			GetBestBlockSubscription => write!(f, "GetBestBlockSubscription"),
			GetFinalizedBlockSubscription => write!(f, "GetFinalizedBlockSubscription"),
		}
	}
}

/// Response types for APIs.
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
	/// Session info
	SessionInfo(Option<polkadot_primitives::SessionInfo>),
	/// Session keys
	SessionAccountKeys(Option<Vec<AccountId32>>),
	/// Session next keys for a validator
	SessionNextKeys(Option<SessionKeys>),
	/// HRMP channels for some parachain (e.g. who are sending messages to us)
	HRMPChannels(BTreeMap<u32, SubxtHrmpChannel>),
	/// HRMP content for a specific channel
	HRMPContent(Vec<Vec<u8>>),
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
	#[error("subxt connection timeout")]
	Timeout,
	#[error("Send failed: {0}")]
	PrioritySendError(#[from] PrioritySendError),
	#[error("Recv failed: {0}")]
	OneshotRecvError(#[from] OneshotRecvError),
	#[error("{0} not found in dynamic storage")]
	EmptyResponseFromDynamicStorage(String),
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
								match self.execute_request(&request, &client).await {
									Ok(v) => {
										let _ = tx.send(v);
									},
									Err(_e) => {
										todo!()
									}
								};
							}
						},
						Err(_) => todo!(),
					}
				}
			}
		}
	}

	async fn execute_request(
		&mut self,
		request: &RpcRequest,
		client: &Box<dyn ApiClientT>,
	) -> color_eyre::Result<RpcResponse, RpcExecutorError> {
		let mut retry = Retry::new(&self.retry);
		loop {
			match self.match_request(request, client).await {
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
		request: &RpcRequest,
		client: &Box<dyn ApiClientT>,
	) -> color_eyre::Result<RpcResponse, RpcExecutorError> {
		use RpcRequest::*;
		use RpcResponse::*;
		let response = match request {
			GetBlockTimestamp(hash) => Timestamp(client.get_block_ts(*hash).await?.unwrap_or_default()),
			// GetHead(tx, maybe_hash) => subxt_get_head(api, maybe_hash).await,
			// GetBlockNumber(tx, maybe_hash) => subxt_get_block_number(api, maybe_hash).await,
			// GetBlockHash(tx, maybe_block_number) => subxt_get_block_hash(api, maybe_block_number).await,
			// GetEvents(tx, hash) => subxt_get_events(api, hash).await,
			// ExtractParaInherent(tx, maybe_hash) => subxt_extract_parainherent(api, maybe_hash).await,
			// GetClaimQueue(tx, hash) => subxt_get_claim_queue(api, hash).await,
			// GetOccupiedCores(tx, hash) => subxt_get_occupied_cores(api, hash).await,
			// GetBackingGroups(tx, hash) => subxt_get_validator_groups(api, hash).await,
			// GetSessionIndex(tx, hash) => subxt_get_session_index(api, hash).await,
			// GetSessionAccountKeys(tx, session_index) => subxt_get_session_account_keys(api, session_index).await,
			// GetSessionNextKeys(tx, ref account) => subxt_get_session_next_keys(api, account).await,
			// GetInboundHRMPChannels(tx, hash, para_id) => subxt_get_inbound_hrmp_channels(api, hash, para_id).await,
			// GetOutboundHRMPChannels(tx, hash, para_id) => subxt_get_outbound_hrmp_channels(api, hash, para_id).await,
			GetHostConfiguration => HostConfiguration(DynamicHostConfiguration::new(
				fetch_dynamic_storage(client, None, "Configuration", "ActiveConfig").await?,
			)),
			GetBestBlockSubscription => ChainSubscription(client.stream_best_block_headers().await?),
			GetFinalizedBlockSubscription => ChainSubscription(client.stream_finalized_block_headers().await?),
			_ => todo!(),
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
			to_backend.send(ExecutorMessage::Rpc(tx, RpcRequest::$request_ty($($arg),*))).await?;
			match rx.await? {
				RpcResponse::$response_ty(res) => Ok(res),
				_ => Err(RpcExecutorError::UnexpectedResponse(RpcRequest::$request_ty($($arg),*))),
			}
		} else {
			Err(RpcExecutorError::ClientNotFound($url.into()))
		}
	};
}

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
	pub fn start(&mut self, url: String) -> color_eyre::Result<tokio::task::JoinHandle<()>, RpcExecutorError> {
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

				Ok(tokio::spawn(fut))
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

	pub async fn get_block_ts(&mut self, url: &str, hash: H256) -> color_eyre::Result<Timestamp, RpcExecutorError> {
		wrap_backend_call!(self, url, GetBlockTimestamp, Timestamp, hash)
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

async fn fetch_dynamic_storage(
	client: &Box<dyn ApiClientT>,
	maybe_hash: Option<H256>,
	pallet_name: &str,
	entry_name: &str,
) -> std::result::Result<Value<u32>, RpcExecutorError> {
	client
		.fetch_dynamic_storage(maybe_hash, pallet_name, entry_name)
		.await?
		.ok_or(RpcExecutorError::EmptyResponseFromDynamicStorage(format!("{pallet_name}.{entry_name}")))
}

pub struct DynamicHostConfiguration(Value<u32>);

impl DynamicHostConfiguration {
	pub fn new(value: Value<u32>) -> Self {
		Self(value)
	}

	pub fn at(&self, field: &str) -> String {
		match self.0.at(field) {
			Some(value) if matches!(value, Value { value: ValueDef::Variant(_), .. }) => match value.at(0) {
				Some(inner) => format!("{}", inner),
				None => format!("{}", 0),
			},
			Some(value) => format!("{}", value),
			None => format!("{}", 0),
		}
	}
}

impl std::fmt::Display for DynamicHostConfiguration {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"\t👀 Max validators: {} / {} per core
\t👍 Needed approvals: {}
\t🥔 No show slots: {}
\t⏳ Delay tranches: {}",
			self.at("max_validators"),
			self.at("max_validators_per_core"),
			self.at("needed_approvals"),
			self.at("no_show_slots"),
			self.at("n_delay_tranches"),
		)
	}
}
