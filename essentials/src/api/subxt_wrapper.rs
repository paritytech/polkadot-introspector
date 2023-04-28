use std::collections::BTreeMap;
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
pub use crate::metadata::polkadot::{
	self, runtime_types as subxt_runtime_types,
	runtime_types::{
		polkadot_core_primitives::CandidateHash,
		polkadot_primitives as polkadot_rt_primitives,
		polkadot_primitives::v2::{AvailabilityBitfield, BackedCandidate, CoreOccupied, ValidatorIndex},
		polkadot_runtime_parachains::{configuration::HostConfiguration, hrmp::HrmpChannel, scheduler::CoreAssignment},
	},
};
use crate::{
	types::{AccountId32, Timestamp, H256},
	utils::{Retry, RetryOptions},
};
use codec::Decode;
use log::error;
use thiserror::Error;

#[cfg(feature = "rococo")]
pub use subxt_runtime_types::rococo_runtime::RuntimeCall as SubxtCall;

#[cfg(feature = "polkadot")]
pub use subxt_runtime_types::polkadot_runtime::RuntimeCall as SubxtCall;

use std::{collections::hash_map::HashMap, fmt::Debug};
use subxt::{OnlineClient, PolkadotConfig};

/// Subxt based APIs for fetching via RPC and processing of extrinsics.
pub enum RequestType {
	/// Returns the block TS from the inherent data.
	GetBlockTimestamp(<PolkadotConfig as subxt::Config>::Hash),
	/// Get a block header.
	GetHead(Option<<PolkadotConfig as subxt::Config>::Hash>),
	/// Get a full block.
	GetBlock(Option<<PolkadotConfig as subxt::Config>::Hash>),
	/// Get block events.
	GetEvents(<PolkadotConfig as subxt::Config>::Hash),
	/// Extract the `ParaInherentData` from a given block.
	ExtractParaInherent(subxt::rpc::types::ChainBlock<PolkadotConfig>),
	/// Get the availability core scheduling information at a given block.
	GetScheduledParas(<PolkadotConfig as subxt::Config>::Hash),
	/// Get occupied core information at a given block.
	GetOccupiedCores(<PolkadotConfig as subxt::Config>::Hash),
	/// Get baking groups at a given block.
	GetBackingGroups(<PolkadotConfig as subxt::Config>::Hash),
	/// Get session index for a specific block.
	GetSessionIndex(<PolkadotConfig as subxt::Config>::Hash),
	/// Get information about specific session.
	GetSessionInfo(u32),
	/// Get information about validators account keys in some session.
	GetSessionAccountKeys(u32),
	/// Get information about inbound HRMP channels, accepts block hash and destination ParaId
	GetInboundHRMPChannels(<PolkadotConfig as subxt::Config>::Hash, u32),
	/// Get data from a specific inbound HRMP channel
	GetHRMPData(<PolkadotConfig as subxt::Config>::Hash, u32, u32),
	/// Get information about inbound HRMP channels, accepts block hash and destination ParaId
	GetOutboundHRMPChannels(<PolkadotConfig as subxt::Config>::Hash, u32),
	/// Get active host configuration
	GetHostConfiguration(()),
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
			RequestType::GetBlock(h) => {
				format!("get block: {:?}", h)
			},
			RequestType::GetEvents(h) => {
				format!("get events: {:?}", h)
			},
			RequestType::ExtractParaInherent(block) => {
				format!("get inherent for block number: {:?}", block.header.number)
			},
			RequestType::GetScheduledParas(h) => {
				format!("get scheduled paras: {:?}", h)
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
			RequestType::GetSessionInfo(id) => {
				format!("get session info: {:?}", id)
			},
			RequestType::GetSessionAccountKeys(id) => {
				format!("get session account keys: {:?}", id)
			},
			RequestType::GetInboundHRMPChannels(h, para_id) => {
				format!("get inbound channels: {:?}; para id: {}", h, para_id)
			},
			RequestType::GetHRMPData(h, src, dst) => {
				format!("get hrmp content: {:?}; src: {}; dst: {}", h, src, dst)
			},
			RequestType::GetOutboundHRMPChannels(h, para_id) => {
				format!("get outbount channels: {:?}; para id: {}", h, para_id)
			},
			RequestType::GetHostConfiguration(_) => "get host configuration".to_string(),
		};
		write!(f, "Subxt request: {}", description)
	}
}

/// The `InherentData` constructed with the subxt API.
pub type InherentData = polkadot_rt_primitives::v2::InherentData<
	subxt_runtime_types::sp_runtime::generic::header::Header<
		::core::primitive::u32,
		subxt_runtime_types::sp_runtime::traits::BlakeTwo256,
	>,
>;

/// Response types for APIs.
pub enum Response {
	/// A timestamp.
	Timestamp(Timestamp),
	/// A block header.
	MaybeHead(Option<<PolkadotConfig as subxt::Config>::Header>),
	/// A full block.
	MaybeBlock(Option<subxt::rpc::types::ChainBlock<PolkadotConfig>>),
	/// Block events
	MaybeEvents(Option<subxt::events::Events<PolkadotConfig>>),
	/// `ParaInherent` data.
	ParaInherentData(InherentData),
	/// Availability core assignments for parachains.
	ScheduledParas(Vec<CoreAssignment>),
	/// List of the occupied availability cores.
	OccupiedCores(Vec<Option<CoreOccupied>>),
	/// Backing validator groups.
	BackingGroups(Vec<Vec<ValidatorIndex>>),
	/// Returns a session index
	SessionIndex(u32),
	/// Session info
	SessionInfo(Option<polkadot_rt_primitives::v2::SessionInfo>),
	/// Session keys
	SessionAccountKeys(Option<Vec<AccountId32>>),
	/// HRMP channels for some parachain (e.g. who are sending messages to us)
	HRMPChannels(BTreeMap<u32, SubxtHrmpChannel>),
	/// HRMP content for a specific channel
	HRMPContent(Vec<Vec<u8>>),
	/// The current host configuration
	HostConfiguration(HostConfiguration<u32>),
}

impl Debug for Response {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Subxt response")
	}
}

/// Represents a pool for subxt requests
#[derive(Clone, Default)]
pub struct RequestExecutor {
	connection_pool: HashMap<String, OnlineClient<PolkadotConfig>>,
	retry: RetryOptions,
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
		Self { retry, ..Default::default() }
	}

	async fn execute_request(&mut self, request: RequestType, url: &str) -> Result {
		let connection_pool = &mut self.connection_pool;
		let maybe_api = connection_pool.get(url).cloned();
		let mut retry = Retry::new(&self.retry);

		loop {
			let api = match maybe_api {
				Some(ref api) => api.clone(),
				None => {
					let new_api = new_client_fn(url, &self.retry).await;
					if let Some(api) = new_api {
						connection_pool.insert(url.to_owned(), api.clone());
						api
					} else {
						return Err(SubxtWrapperError::ConnectionError)
					}
				},
			};
			let reply = match request {
				RequestType::GetBlockTimestamp(hash) => subxt_get_block_ts(&api, hash).await,
				RequestType::GetHead(maybe_hash) => subxt_get_head(&api, maybe_hash).await,
				RequestType::GetBlock(maybe_hash) => subxt_get_block(&api, maybe_hash).await,
				RequestType::GetEvents(hash) => subxt_get_events(&api, hash).await,
				RequestType::ExtractParaInherent(ref block) => subxt_extract_parainherent(block),
				RequestType::GetScheduledParas(hash) => subxt_get_sheduled_paras(&api, hash).await,
				RequestType::GetOccupiedCores(hash) => subxt_get_occupied_cores(&api, hash).await,
				RequestType::GetBackingGroups(hash) => subxt_get_validator_groups(&api, hash).await,
				RequestType::GetSessionIndex(hash) => subxt_get_session_index(&api, hash).await,
				RequestType::GetSessionInfo(session_index) => subxt_get_session_info(&api, session_index).await,
				RequestType::GetSessionAccountKeys(session_index) =>
					subxt_get_session_account_keys(&api, session_index).await,
				RequestType::GetInboundHRMPChannels(hash, para_id) =>
					subxt_get_inbound_hrmp_channels(&api, hash, para_id).await,
				RequestType::GetOutboundHRMPChannels(hash, para_id) =>
					subxt_get_outbound_hrmp_channels(&api, hash, para_id).await,
				RequestType::GetHRMPData(hash, para_id, sender) =>
					subxt_get_hrmp_content(&api, hash, para_id, sender).await,
				RequestType::GetHostConfiguration(_) => subxt_get_host_configuration(&api).await,
			};

			match reply {
				Ok(rep) => return Ok(rep),
				Err(err) => match &err {
					SubxtWrapperError::SubxtError(subxt::Error::Io(io_err)) => {
						connection_pool.remove(url);
						error!("[{}] Subxt IO error: {:?}", url, io_err);
						if (retry.sleep().await).is_err() {
							return Err(SubxtWrapperError::Timeout)
						}
					},
					_ => return Err(err),
				},
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

	pub async fn get_block(
		&mut self,
		url: &str,
		maybe_hash: Option<<PolkadotConfig as subxt::Config>::Hash>,
	) -> std::result::Result<Option<subxt::rpc::types::ChainBlock<PolkadotConfig>>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetBlock, MaybeBlock, url, maybe_hash)
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
	) -> std::result::Result<Option<InherentData>, SubxtWrapperError> {
		match self.get_block(url, maybe_hash).await {
			Ok(Some(block)) => match self.execute_request(RequestType::ExtractParaInherent(block), url).await {
				Ok(Response::ParaInherentData(data)) => Ok(Some(data)),
				Err(e) => Err(e),
				_ => panic!("Expected ParaInherentData, got something else."),
			},
			Ok(None) => Ok(None),
			Err(e) => Err(e),
		}
	}

	pub async fn get_scheduled_paras(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Vec<CoreAssignment>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetScheduledParas, ScheduledParas, url, block_hash)
	}

	pub async fn get_occupied_cores(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Vec<Option<CoreOccupied>>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetOccupiedCores, OccupiedCores, url, block_hash)
	}

	pub async fn get_backing_groups(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
	) -> std::result::Result<Vec<Vec<ValidatorIndex>>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetBackingGroups, BackingGroups, url, block_hash)
	}

	pub async fn get_session_info(
		&mut self,
		url: &str,
		session_index: u32,
	) -> std::result::Result<Option<polkadot_rt_primitives::v2::SessionInfo>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetSessionInfo, SessionInfo, url, session_index)
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

	pub async fn get_hrmp_content(
		&mut self,
		url: &str,
		block_hash: <PolkadotConfig as subxt::Config>::Hash,
		receiver_id: u32,
		sender_id: u32,
	) -> std::result::Result<Vec<Vec<u8>>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetHRMPData, HRMPContent, url, block_hash, receiver_id, sender_id)
	}

	pub async fn get_host_configuration(
		&mut self,
		url: &str,
	) -> std::result::Result<HostConfiguration<u32>, SubxtWrapperError> {
		wrap_subxt_call!(self, GetHostConfiguration, HostConfiguration, url, ())
	}
}

// Attempts to connect to websocket and returns an RuntimeApi instance if successful.
async fn new_client_fn(url: &str, retry: &RetryOptions) -> Option<OnlineClient<PolkadotConfig>> {
	let mut retry = Retry::new(retry);

	loop {
		match OnlineClient::<PolkadotConfig>::from_url(url.to_owned()).await {
			Ok(api) => return Some(api),
			Err(err) => {
				error!("[{}] Client error: {:?}", url, err);
				if (retry.sleep().await).is_err() {
					return None
				}
			},
		};
	}
}

async fn subxt_get_head(api: &OnlineClient<PolkadotConfig>, maybe_hash: Option<H256>) -> Result {
	Ok(Response::MaybeHead(api.rpc().header(maybe_hash).await?))
}

async fn subxt_get_block_ts(api: &OnlineClient<PolkadotConfig>, hash: H256) -> Result {
	let timestamp = polkadot::storage().timestamp().now();
	Ok(Response::Timestamp(api.storage().at(hash).fetch(&timestamp).await?.unwrap_or_default()))
}

async fn subxt_get_block(api: &OnlineClient<PolkadotConfig>, maybe_hash: Option<H256>) -> Result {
	Ok(Response::MaybeBlock(api.rpc().block(maybe_hash).await?.map(|response| response.block)))
}

async fn subxt_get_events(api: &OnlineClient<PolkadotConfig>, hash: H256) -> Result {
	Ok(Response::MaybeEvents(Some(api.events().at(hash).await?)))
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

async fn subxt_get_sheduled_paras(api: &OnlineClient<PolkadotConfig>, block_hash: H256) -> Result {
	let addr = polkadot::storage().para_scheduler().scheduled();
	let scheduled_paras = api.storage().at(block_hash).fetch(&addr).await?.unwrap_or_default();
	Ok(Response::ScheduledParas(scheduled_paras))
}

async fn subxt_get_occupied_cores(api: &OnlineClient<PolkadotConfig>, block_hash: H256) -> Result {
	let addr = polkadot::storage().para_scheduler().availability_cores();
	let occupied_cores = api.storage().at(block_hash).fetch(&addr).await?.unwrap_or_default();
	Ok(Response::OccupiedCores(occupied_cores))
}

async fn subxt_get_validator_groups(api: &OnlineClient<PolkadotConfig>, block_hash: H256) -> Result {
	let addr = polkadot::storage().para_scheduler().validator_groups();
	let groups = api.storage().at(block_hash).fetch(&addr).await?.unwrap_or_default();
	Ok(Response::BackingGroups(groups))
}

async fn subxt_get_session_index(api: &OnlineClient<PolkadotConfig>, block_hash: H256) -> Result {
	let addr = polkadot::storage().session().current_index();
	let session_index = api.storage().at(block_hash).fetch(&addr).await?.unwrap_or_default();
	Ok(Response::SessionIndex(session_index))
}

async fn subxt_get_session_info(api: &OnlineClient<PolkadotConfig>, session_index: u32) -> Result {
	let addr = polkadot::storage().para_session_info().sessions(session_index);
	let session_info = api.storage().at_latest().await?.fetch(&addr).await?;
	Ok(Response::SessionInfo(session_info))
}

async fn subxt_get_session_account_keys(api: &OnlineClient<PolkadotConfig>, session_index: u32) -> Result {
	let addr = polkadot::storage().para_session_info().account_keys(session_index);
	let session_keys = api.storage().at_latest().await?.fetch(&addr).await?;
	Ok(Response::SessionAccountKeys(session_keys))
}

/// A wrapper over subxt HRMP channel configuration
#[derive(Debug, Clone)]
pub struct SubxtHrmpChannel {
	pub max_capacity: u32,
	pub max_total_size: u32,
	pub max_message_size: u32,
	pub msg_count: u32,
	pub total_size: u32,
	pub mqc_head: Option<H256>,
	pub sender_deposit: u128,
	pub recipient_deposit: u128,
}

impl From<HrmpChannel> for SubxtHrmpChannel {
	fn from(channel: HrmpChannel) -> Self {
		SubxtHrmpChannel {
			max_capacity: channel.max_capacity,
			max_total_size: channel.max_total_size,
			max_message_size: channel.max_message_size,
			msg_count: channel.msg_count,
			total_size: channel.total_size,
			mqc_head: channel.mqc_head,
			sender_deposit: channel.sender_deposit,
			recipient_deposit: channel.recipient_deposit,
		}
	}
}

async fn subxt_get_inbound_hrmp_channels(api: &OnlineClient<PolkadotConfig>, block_hash: H256, para_id: u32) -> Result {
	use subxt_runtime_types::polkadot_parachain::primitives::{HrmpChannelId, Id};
	let addr = polkadot::storage().hrmp().hrmp_ingress_channels_index(&Id(para_id));
	let hrmp_channels = api.storage().at(block_hash).fetch(&addr).await?.unwrap_or_default();
	let mut channels_configuration: BTreeMap<u32, SubxtHrmpChannel> = BTreeMap::new();
	for peer_parachain_id in hrmp_channels.into_iter().map(|id| id.0) {
		let id = HrmpChannelId { sender: Id(peer_parachain_id), recipient: Id(para_id) };
		let addr = polkadot::storage().hrmp().hrmp_channels(&id);
		api.storage()
			.at(block_hash)
			.fetch(&addr)
			.await?
			.map(|hrmp_channel_configuration| {
				channels_configuration.insert(peer_parachain_id, hrmp_channel_configuration.into())
			});
	}
	Ok(Response::HRMPChannels(channels_configuration))
}

async fn subxt_get_outbound_hrmp_channels(
	api: &OnlineClient<PolkadotConfig>,
	block_hash: H256,
	para_id: u32,
) -> Result {
	use subxt_runtime_types::polkadot_parachain::primitives::{HrmpChannelId, Id};

	let addr = polkadot::storage().hrmp().hrmp_egress_channels_index(&Id(para_id));
	let hrmp_channels = api.storage().at(block_hash).fetch(&addr).await?.unwrap_or_default();
	let mut channels_configuration: BTreeMap<u32, SubxtHrmpChannel> = BTreeMap::new();
	for peer_parachain_id in hrmp_channels.into_iter().map(|id| id.0) {
		let id = HrmpChannelId { sender: Id(peer_parachain_id), recipient: Id(para_id) };
		let addr = polkadot::storage().hrmp().hrmp_channels(&id);
		api.storage()
			.at(block_hash)
			.fetch(&addr)
			.await?
			.map(|hrmp_channel_configuration| {
				channels_configuration.insert(peer_parachain_id, hrmp_channel_configuration.into())
			});
	}
	Ok(Response::HRMPChannels(channels_configuration))
}

async fn subxt_get_hrmp_content(
	api: &OnlineClient<PolkadotConfig>,
	block_hash: H256,
	receiver: u32,
	sender: u32,
) -> Result {
	use subxt_runtime_types::polkadot_parachain::primitives::{HrmpChannelId, Id};

	let id = HrmpChannelId { sender: Id(sender), recipient: Id(receiver) };
	let addr = polkadot::storage().hrmp().hrmp_channel_contents(&id);
	let hrmp_content = api.storage().at(block_hash).fetch(&addr).await?.unwrap_or_default();
	Ok(Response::HRMPContent(hrmp_content.into_iter().map(|hrmp_content| hrmp_content.data).collect()))
}

async fn subxt_get_host_configuration(api: &OnlineClient<PolkadotConfig>) -> Result {
	let addr = polkadot::storage().configuration().active_config();
	let host_configuration = api.storage().at_latest().await?.fetch(&addr).await?.unwrap();
	Ok(Response::HostConfiguration(host_configuration))
}

fn subxt_extract_parainherent(block: &subxt::rpc::types::ChainBlock<PolkadotConfig>) -> Result {
	// `ParaInherent` data is always at index #1.
	let bytes = &block.extrinsics[1].0;

	let data = match decode_extrinsic(&mut bytes.as_slice()).expect("Failed to decode `ParaInherent`") {
		SubxtCall::ParaInherent(
			subxt_runtime_types::polkadot_runtime_parachains::paras_inherent::pallet::Call::enter { data },
		) => data,
		_ => unimplemented!("Unhandled variant"),
	};
	Ok(Response::ParaInherentData(data))
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
}
pub type Result = std::result::Result<Response, SubxtWrapperError>;
