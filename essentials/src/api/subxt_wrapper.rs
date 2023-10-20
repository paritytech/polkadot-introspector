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

use crate::{
	api::{
		api_client::{build_online_client, ApiClientT, HeaderStream},
		dynamic::{decode_availability_cores, decode_claim_queue, decode_scheduled_paras, decode_validator_groups},
	},
	metadata::polkadot_primitives,
	types::{
		AccountId32, BlockNumber, ClaimQueue, CoreAssignment, CoreOccupied, InherentData, SessionKeys,
		SubxtHrmpChannel, Timestamp, H256,
	},
	utils::{Retry, RetryOptions},
};
use log::{error, warn};
use std::{
	collections::{hash_map::HashMap, BTreeMap},
	fmt::Debug,
};
use subxt::{
	dynamic::{At, Value},
	ext::scale_value::ValueDef,
	PolkadotConfig,
};
use thiserror::Error;

/// Subxt based APIs for fetching via RPC and processing of extrinsics.
pub enum RequestType {
	/// Returns the block TS from the inherent data.
	GetBlockTimestamp(<PolkadotConfig as subxt::Config>::Hash),
	/// Get a block header.
	GetHead(Option<<PolkadotConfig as subxt::Config>::Hash>),
	/// Get a full block.
	GetBlockNumber(Option<<PolkadotConfig as subxt::Config>::Hash>),
	/// Get a block hash.
	GetBlockHash(Option<BlockNumber>),
	/// Get block events.
	GetEvents(<PolkadotConfig as subxt::Config>::Hash),
	/// Extract the `ParaInherentData` from a given block.
	ExtractParaInherent(Option<<PolkadotConfig as subxt::Config>::Hash>),
	/// Get the availability core scheduling information at a given block.
	GetScheduledParas(<PolkadotConfig as subxt::Config>::Hash),
	/// Get the claim queue scheduling information at a given block.
	GetClaimQueue(<PolkadotConfig as subxt::Config>::Hash),
	/// Get occupied core information at a given block.
	GetOccupiedCores(<PolkadotConfig as subxt::Config>::Hash),
	/// Get baking groups at a given block.
	GetBackingGroups(<PolkadotConfig as subxt::Config>::Hash),
	/// Get session index for a specific block.
	GetSessionIndex(<PolkadotConfig as subxt::Config>::Hash),
	/// Get information about validators account keys in some session.
	GetSessionAccountKeys(u32),
	/// Get information about validator's next session keys.
	GetSessionNextKeys(AccountId32),
	/// Get information about inbound HRMP channels, accepts block hash and destination ParaId
	GetInboundHRMPChannels(<PolkadotConfig as subxt::Config>::Hash, u32),
	/// Get information about inbound HRMP channels, accepts block hash and destination ParaId
	GetOutboundHRMPChannels(<PolkadotConfig as subxt::Config>::Hash, u32),
	/// Get active host configuration
	GetHostConfiguration(()),
	/// Get a subscription to the best blocks chain
	GetBestBlockSubscription(()),
	/// Get a subscription to the finalized blocks chain
	GetFinalizedBlockSubscription(()),
}

// Required after subxt changes that removed Debug trait from the generated structures
impl Debug for RequestType {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let description = match self {
			RequestType::GetBlockTimestamp(h) => {
				format!("get block timestamp: {:?}", h)
			},
			RequestType::GetHead(h) => {
				format!("get head: {:?}", h)
			},
			RequestType::GetBlockNumber(h) => {
				format!("get block: {:?}", h)
			},
			RequestType::GetBlockHash(h) => {
				format!("get block hash: {:?}", h)
			},
			RequestType::GetEvents(h) => {
				format!("get events: {:?}", h)
			},
			RequestType::ExtractParaInherent(h) => {
				format!("get inherent for block: {:?}", h)
			},
			RequestType::GetScheduledParas(h) => {
				format!("get scheduled paras: {:?}", h)
			},
			RequestType::GetClaimQueue(h) => {
				format!("get claim queue: {:?}", h)
			},
			RequestType::GetOccupiedCores(h) => {
				format!("get occupied cores: {:?}", h)
			},
			RequestType::GetBackingGroups(h) => {
				format!("get backing groups: {:?}", h)
			},
			RequestType::GetSessionIndex(h) => {
				format!("get session index: {:?}", h)
			},
			RequestType::GetSessionAccountKeys(id) => {
				format!("get session account keys: {:?}", id)
			},
			RequestType::GetSessionNextKeys(account) => {
				format!("get next session account keys: {:?}", account)
			},
			RequestType::GetInboundHRMPChannels(h, para_id) => {
				format!("get inbound channels: {:?}; para id: {}", h, para_id)
			},
			RequestType::GetOutboundHRMPChannels(h, para_id) => {
				format!("get outbount channels: {:?}; para id: {}", h, para_id)
			},
			RequestType::GetHostConfiguration(_) => "get host configuration".to_string(),
			RequestType::GetBestBlockSubscription(_) => "get best block subscription".to_string(),
			RequestType::GetFinalizedBlockSubscription(_) => "get finalized block subscription".to_string(),
		};
		write!(f, "Subxt request: {}", description)
	}
}

/// Response types for APIs.
pub enum Response {
	/// A timestamp.
	Timestamp(Timestamp),
	/// A block header.
	MaybeHead(Option<<PolkadotConfig as subxt::Config>::Header>),
	/// A full block.
	BlockNumber(BlockNumber),
	/// A block hash.
	MaybeBlockHash(Option<H256>),
	/// Block events
	MaybeEvents(Option<subxt::events::Events<PolkadotConfig>>),
	/// `ParaInherent` data.
	ParaInherentData(InherentData),
	/// Availability core assignments for parachains.
	ScheduledParas(Vec<CoreAssignment>),
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

impl Debug for Response {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Subxt response")
	}
}

/// Represents a pool for subxt requests
#[derive(Clone)]
pub struct RequestExecutor {
	connection_pool: HashMap<String, Box<dyn ApiClientT>>,
	retry: RetryOptions,
}

impl Clone for Box<dyn ApiClientT> {
	fn clone(&self) -> Self {
		dyn_clone::clone_box(&**self)
	}
}

macro_rules! wrap_subxt_call {
	($self: ident, $req_ty: ident, $rep_ty: ident, $url: ident, $($arg:expr),*) => (
		match $self.execute_request(RequestType::$req_ty($($arg),*), $url).await {
			Ok(Response::$rep_ty(data)) => Ok(data),
			Err(e) => Err(e),
			_ => panic!("Expected {}, got something else.", stringify!($rep_ty)),
		}
	)
}

impl RequestExecutor {
	pub fn new(retry: RetryOptions) -> Self {
		Self { retry, connection_pool: HashMap::new() }
	}

	async fn execute_request(&mut self, request: RequestType, url: &str) -> Result {
		let connection_pool = &mut self.connection_pool;
		let mut retry = Retry::new(&self.retry);

		loop {
			let api: Box<dyn ApiClientT> = match connection_pool.get(url) {
				Some(api) => dyn_clone::clone_box(&**api),
				None => {
					let new_api = new_client_fn(url, &self.retry).await;
					if let Some(api) = new_api {
						connection_pool.insert(url.to_owned(), dyn_clone::clone_box(&*api));
						api
					} else {
						return Err(SubxtWrapperError::ConnectionError)
					}
				},
			};
			let reply = match request {
				RequestType::GetBlockTimestamp(hash) => subxt_get_block_ts(api, hash).await,
				RequestType::GetHead(maybe_hash) => subxt_get_head(api, maybe_hash).await,
				RequestType::GetBlockNumber(maybe_hash) => subxt_get_block_number(api, maybe_hash).await,
				RequestType::GetBlockHash(maybe_block_number) => subxt_get_block_hash(api, maybe_block_number).await,
				RequestType::GetEvents(hash) => subxt_get_events(api, hash).await,
				RequestType::ExtractParaInherent(maybe_hash) => subxt_extract_parainherent(api, maybe_hash).await,
				RequestType::GetScheduledParas(hash) => subxt_get_sheduled_paras(api, hash).await,
				RequestType::GetClaimQueue(hash) => subxt_get_claim_queue(api, hash).await,
				RequestType::GetOccupiedCores(hash) => subxt_get_occupied_cores(api, hash).await,
				RequestType::GetBackingGroups(hash) => subxt_get_validator_groups(api, hash).await,
				RequestType::GetSessionIndex(hash) => subxt_get_session_index(api, hash).await,
				RequestType::GetSessionAccountKeys(session_index) =>
					subxt_get_session_account_keys(api, session_index).await,
				RequestType::GetSessionNextKeys(ref account) => subxt_get_session_next_keys(api, account).await,
				RequestType::GetInboundHRMPChannels(hash, para_id) =>
					subxt_get_inbound_hrmp_channels(api, hash, para_id).await,
				RequestType::GetOutboundHRMPChannels(hash, para_id) =>
					subxt_get_outbound_hrmp_channels(api, hash, para_id).await,
				RequestType::GetHostConfiguration(_) => subxt_get_host_configuration(api).await,
				RequestType::GetBestBlockSubscription(_) => subxt_get_best_block_subscription(api).await,
				RequestType::GetFinalizedBlockSubscription(_) => subxt_get_finalized_block_subscription(api).await,
			};

			if let Err(e) = reply {
				let need_to_retry = matches!(
					e,
					SubxtWrapperError::SubxtError(subxt::Error::Io(_)) |
						SubxtWrapperError::SubxtError(subxt::Error::Rpc(_))
				);
				if !need_to_retry {
					return Err(e)
				}
				warn!("[{}] Subxt error: {:?}", url, e);
				connection_pool.remove(url);
				if (retry.sleep().await).is_err() {
					return Err(SubxtWrapperError::Timeout)
				}
			} else {
				return reply
			}
		}
	}

	pub async fn get_block_timestamp(
		&mut self,
		url: &str,
		hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Timestamp, SubxtWrapperError> {
		wrap_subxt_call!(self, GetBlockTimestamp, Timestamp, url, hash)
	}

	pub async fn get_block_head(
		&mut self,
		url: &str,
		maybe_hash: Option<<PolkadotConfig as subxt::Config>::Hash>,
	) -> std::result::Result<Option<<PolkadotConfig as subxt::Config>::Header>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetHead, MaybeHead, url, maybe_hash)
	}

	pub async fn get_block_number(
		&mut self,
		url: &str,
		maybe_hash: Option<<PolkadotConfig as subxt::Config>::Hash>,
	) -> std::result::Result<BlockNumber, SubxtWrapperError> {
		wrap_subxt_call!(self, GetBlockNumber, BlockNumber, url, maybe_hash)
	}

	pub async fn get_block_hash(
		&mut self,
		url: &str,
		maybe_block_number: Option<BlockNumber>,
	) -> std::result::Result<Option<H256>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetBlockHash, MaybeBlockHash, url, maybe_block_number)
	}

	pub async fn get_events(
		&mut self,
		url: &str,
		hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Option<subxt::events::Events<PolkadotConfig>>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetEvents, MaybeEvents, url, hash)
	}

	pub async fn extract_parainherent_data(
		&mut self,
		url: &str,
		maybe_hash: Option<<PolkadotConfig as subxt::Config>::Hash>,
	) -> std::result::Result<InherentData, SubxtWrapperError> {
		wrap_subxt_call!(self, ExtractParaInherent, ParaInherentData, url, maybe_hash)
	}

	pub async fn get_scheduled_paras(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Vec<CoreAssignment>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetScheduledParas, ScheduledParas, url, block_hash)
	}

	pub async fn get_claim_queue(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<ClaimQueue, SubxtWrapperError> {
		wrap_subxt_call!(self, GetClaimQueue, ClaimQueue, url, block_hash)
	}

	pub async fn get_occupied_cores(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Vec<CoreOccupied>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetOccupiedCores, OccupiedCores, url, block_hash)
	}

	pub async fn get_backing_groups(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Vec<Vec<polkadot_primitives::ValidatorIndex>>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetBackingGroups, BackingGroups, url, block_hash)
	}

	pub async fn get_session_index(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<u32, SubxtWrapperError> {
		wrap_subxt_call!(self, GetSessionIndex, SessionIndex, url, block_hash)
	}

	pub async fn get_session_account_keys(
		&mut self,
		url: &str,
		session_index: u32,
	) -> std::result::Result<Option<Vec<AccountId32>>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetSessionAccountKeys, SessionAccountKeys, url, session_index)
	}

	pub async fn get_session_next_keys(
		&mut self,
		url: &str,
		account: AccountId32,
	) -> std::result::Result<Option<SessionKeys>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetSessionNextKeys, SessionNextKeys, url, account)
	}

	pub async fn get_inbound_hrmp_channels(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
		para_id: u32,
	) -> std::result::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetInboundHRMPChannels, HRMPChannels, url, block_hash, para_id)
	}

	pub async fn get_outbound_hrmp_channels(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
		para_id: u32,
	) -> std::result::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetOutboundHRMPChannels, HRMPChannels, url, block_hash, para_id)
	}

	pub async fn get_host_configuration(
		&mut self,
		url: &str,
	) -> std::result::Result<DynamicHostConfiguration, SubxtWrapperError> {
		wrap_subxt_call!(self, GetHostConfiguration, HostConfiguration, url, ())
	}

	pub async fn get_best_block_subscription(
		&mut self,
		url: &str,
	) -> std::result::Result<HeaderStream, SubxtWrapperError> {
		wrap_subxt_call!(self, GetBestBlockSubscription, ChainSubscription, url, ())
	}

	pub async fn get_finalized_block_subscription(
		&mut self,
		url: &str,
	) -> std::result::Result<HeaderStream, SubxtWrapperError> {
		wrap_subxt_call!(self, GetFinalizedBlockSubscription, ChainSubscription, url, ())
	}
}

// Attempts to connect to websocket and returns an RuntimeApi instance if successful.
async fn new_client_fn(url: &str, retry: &RetryOptions) -> Option<Box<dyn ApiClientT>> {
	let mut retry = Retry::new(retry);

	loop {
		match build_online_client(url).await {
			Ok(client) => return Some(Box::new(client)),
			Err(err) => {
				error!("[{}] Client error: {:?}", url, err);
				if (retry.sleep().await).is_err() {
					return None
				}
			},
		};
	}
}

async fn subxt_get_head(api: Box<dyn ApiClientT>, maybe_hash: Option<H256>) -> Result {
	let head = match api.get_head(maybe_hash).await {
		Ok(v) => Some(v),
		Err(subxt::Error::Block(subxt::error::BlockError::NotFound(_))) => None,
		Err(err) => return Err(err.into()),
	};

	Ok(Response::MaybeHead(head))
}

async fn subxt_get_block_ts(api: Box<dyn ApiClientT>, hash: H256) -> Result {
	Ok(Response::Timestamp(api.get_block_ts(hash).await?.unwrap_or_default()))
}

async fn subxt_get_block_number(api: Box<dyn ApiClientT>, maybe_hash: Option<H256>) -> Result {
	Ok(Response::BlockNumber(api.get_block_number(maybe_hash).await?))
}

async fn subxt_get_block_hash(api: Box<dyn ApiClientT>, maybe_block_number: Option<BlockNumber>) -> Result {
	Ok(Response::MaybeBlockHash(api.legacy_get_block_hash(maybe_block_number).await?))
}

async fn subxt_get_events(api: Box<dyn ApiClientT>, hash: H256) -> Result {
	Ok(Response::MaybeEvents(Some(api.get_events(hash).await?)))
}

/// Error originated from decoding an extrinsic.
#[derive(Clone, Debug)]
pub enum DecodeExtrinsicError {
	/// Expected more data.
	EarlyEof,
	/// Unsupported extrinsic.
	Unsupported,
	/// Failed to decode.
	CodecError(parity_scale_codec::Error),
}

async fn fetch_dynamic_storage(
	api: Box<dyn ApiClientT>,
	maybe_hash: Option<H256>,
	pallet_name: &str,
	entry_name: &str,
) -> std::result::Result<Value<u32>, SubxtWrapperError> {
	api.fetch_dynamic_storage(maybe_hash, pallet_name, entry_name)
		.await?
		.ok_or(SubxtWrapperError::EmptyResponseFromDynamicStorage(format!("{pallet_name}.{entry_name}")))
}

async fn subxt_get_sheduled_paras(api: Box<dyn ApiClientT>, block_hash: H256) -> Result {
	let value = fetch_dynamic_storage(api, Some(block_hash), "ParaScheduler", "Scheduled").await?;
	let paras = decode_scheduled_paras(&value)?;

	Ok(Response::ScheduledParas(paras))
}

async fn subxt_get_claim_queue(api: Box<dyn ApiClientT>, block_hash: H256) -> Result {
	let value = fetch_dynamic_storage(api, Some(block_hash), "ParaScheduler", "ClaimQueue").await?;
	let queue = decode_claim_queue(&value)?;

	Ok(Response::ClaimQueue(queue))
}

async fn subxt_get_occupied_cores(api: Box<dyn ApiClientT>, block_hash: H256) -> Result {
	let value = fetch_dynamic_storage(api, Some(block_hash), "ParaScheduler", "AvailabilityCores").await?;
	let cores = decode_availability_cores(&value)?;

	Ok(Response::OccupiedCores(cores))
}

async fn subxt_get_validator_groups(api: Box<dyn ApiClientT>, block_hash: H256) -> Result {
	let value = fetch_dynamic_storage(api, Some(block_hash), "ParaScheduler", "ValidatorGroups").await?;
	let groups = decode_validator_groups(&value)?;

	Ok(Response::BackingGroups(groups))
}

async fn subxt_get_session_index(api: Box<dyn ApiClientT>, block_hash: H256) -> Result {
	Ok(Response::SessionIndex(api.get_session_index(block_hash).await?.unwrap_or_default()))
}

async fn subxt_get_session_account_keys(api: Box<dyn ApiClientT>, session_index: u32) -> Result {
	Ok(Response::SessionAccountKeys(api.get_session_account_keys(session_index).await?))
}

async fn subxt_get_session_next_keys(api: Box<dyn ApiClientT>, account: &AccountId32) -> Result {
	Ok(Response::SessionNextKeys(api.get_session_next_keys(account).await?))
}

async fn subxt_get_inbound_hrmp_channels(api: Box<dyn ApiClientT>, block_hash: H256, para_id: u32) -> Result {
	Ok(Response::HRMPChannels(api.get_inbound_hrmp_channels(block_hash, para_id).await?))
}

async fn subxt_get_outbound_hrmp_channels(api: Box<dyn ApiClientT>, block_hash: H256, para_id: u32) -> Result {
	Ok(Response::HRMPChannels(api.get_outbound_hrmp_channels(block_hash, para_id).await?))
}

async fn subxt_get_host_configuration(api: Box<dyn ApiClientT>) -> Result {
	let value = fetch_dynamic_storage(api, None, "Configuration", "ActiveConfig").await?;
	Ok(Response::HostConfiguration(DynamicHostConfiguration::new(value)))
}

async fn subxt_get_best_block_subscription(api: Box<dyn ApiClientT>) -> Result {
	Ok(Response::ChainSubscription(api.stream_best_block_headers().await?))
}

async fn subxt_get_finalized_block_subscription(api: Box<dyn ApiClientT>) -> Result {
	Ok(Response::ChainSubscription(api.stream_finalized_block_headers().await?))
}

async fn subxt_extract_parainherent(api: Box<dyn ApiClientT>, maybe_hash: Option<H256>) -> Result {
	Ok(Response::ParaInherentData(api.extract_parainherent(maybe_hash).await?))
}

#[derive(Debug, Error)]
pub enum SubxtWrapperError {
	#[error("subxt error: {0}")]
	SubxtError(#[from] subxt::error::Error),
	#[error("subxt connection timeout")]
	Timeout,
	#[error("subxt connection error")]
	ConnectionError,
	#[error("decode extinisc error")]
	DecodeExtrinsicError,
	#[error("{0} not found in dynamic storage")]
	EmptyResponseFromDynamicStorage(String),
	#[error("decode dynamic value error: expected `{0}`, got {1}")]
	DecodeDynamicError(String, ValueDef<u32>),
}
pub type Result = std::result::Result<Response, SubxtWrapperError>;

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
