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

pub mod candidate_record;
mod ws;

use crate::{
	api::{
		ApiService,
		executor::{RequestExecutor, RequestExecutorError},
	},
	chain_events::{
		ChainEvent, SubxtCandidateEvent, SubxtCandidateEventType, SubxtDispute, SubxtDisputeResult, decode_chain_event,
	},
	chain_subscription::ChainSubscriptionEvent,
	init::Shutdown,
	metadata::polkadot_primitives::DisputeStatement,
	storage::{RecordTime, RecordsStorageConfig, StorageEntry},
	types::{ClaimQueue, H256, Header, InherentData, OnDemandOrder, PolkadotHasher, Timestamp},
};
use candidate_record::{CandidateDisputed, CandidateInclusionRecord, CandidateRecord, DisputeResult};
use clap::{Parser, ValueEnum};
use color_eyre::eyre::eyre;
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use parity_scale_codec::{Decode, Encode};
use polkadot_introspector_priority_channel::{
	Receiver, Sender, channel as priority_channel, channel_with_capacities as priority_channel_with_capacities,
};
use std::{
	cmp::Ordering,
	collections::BTreeMap,
	default::Default,
	hash::Hash,
	net::SocketAddr,
	time::{Duration, SystemTime, UNIX_EPOCH},
};
use subxt::{PolkadotConfig, config::Hasher};
use thiserror::Error;
use tokio::sync::broadcast::Sender as BroadcastSender;
use ws::{WebSocketEventType, WebSocketListener, WebSocketListenerConfig, WebSocketUpdateEvent};

/// Used for bulk messages in the normal channels
pub const COLLECTOR_NORMAL_CHANNEL_CAPACITY: usize = 32;
/// Used for bulk messages in the broadcast channels
pub const COLLECTOR_BROADCAST_CHANNEL_CAPACITY: usize = 512;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub struct CollectorOptions {
	/// Maximum blocks to store
	#[clap(name = "max-blocks", long)]
	max_blocks: Option<usize>,
	/// WS listen address to bind to
	#[clap(short = 'l', long = "listen")]
	listen_addr: Option<SocketAddr>,
	/// Collect metrics for HRMP channels
	#[clap(long = "channels", default_value_t = false)]
	pub hrmp_channels: bool,
	#[clap(short = 's', long = "subscribe-mode", default_value_t, value_enum)]
	pub subscribe_mode: CollectorSubscribeMode,
	/// Evict a stalled parachain after this amount of skipped blocks
	#[clap(long, default_value = "256")]
	max_parachain_stall: u32,
}

/// How to subscribe to subxt blocks
#[derive(strum::Display, Debug, Clone, Copy, ValueEnum, Default)]
pub enum CollectorSubscribeMode {
	/// Subscribe to the best chain
	Best,
	/// Subscribe to finalized blocks
	#[default]
	Finalized,
}

/// This type is used to distinguish different keys in the storage
#[derive(Clone, Copy, Debug, Hash, Ord, PartialOrd, Eq, PartialEq)]
pub enum CollectorPrefixType {
	/// A block's timestamp
	Timestamp,
	/// Candidate prefixed by Parachain-Id
	Candidate(u32),
	/// A mapping to find out parachain id by candidate hash (e.g. when we don't know the parachain)
	CandidatesParachains,
	/// Relay chain block header
	RelayBlockHeader,
	/// Last finalized relay chain block number when new head appeared
	RelevantFinalizedBlockNumber,
	/// Validators account keys keyed by session index hash (blake2b(session_index))
	AccountKeys,
	/// Core assignments
	CoreAssignments,
	/// Occupied cores
	OccupiedCores,
	/// Backing groups
	BackingGroups,
	/// Inherent data (more expensive to store, so good to have it shared)
	InherentData,
	/// Dispute information indexed by Parachain-Id; data is DisputeInfo
	Dispute(u32),
	/// On-demand order information by parachain id
	OnDemandOrder(u32),
	/// Inbound/Outbound HRMP channel configuration
	InboundOutboundHrmpChannels(u32),
}

/// A type that defines prefix + hash itself
pub type CollectorStorageApi = ApiService<H256, CollectorPrefixType>;

/// A structure used to track disputes progress
#[derive(Clone, Debug, Encode, Decode)]
pub struct DisputeInfo {
	pub initiated: u32,
	pub initiator_indices: Vec<u32>,
	pub session_index: u32,
	pub dispute: SubxtDispute,
	pub parachain_id: u32,
	pub outcome: Option<SubxtDisputeResult>,
	pub concluded: Option<u32>,
}

/// The current state of the collector, used to detect forks and track candidates
/// and other events without query storage
#[derive(Default)]
struct CollectorState {
	/// The current block number
	current_relay_chain_block_number: u32,
	/// A list of hashes at this height (e.g. if we see forks)
	current_relay_chain_block_hashes: Vec<H256>,
	/// A list of candidates seen, indexed by parachain id
	candidates_seen: BTreeMap<u32, Vec<H256>>,
	/// A list of disputes seen, indexed by parachain id
	disputes_seen: BTreeMap<u32, Vec<DisputeInfo>>,
	/// A list of candidates that have been backed in the current block
	candidates_backed: Vec<BackedCandidateInfo>,
	/// A list of candidate hashes that have been included in the current block
	candidates_included: Vec<H256>,
	/// A list of candidate hashes that have timed out in the current block
	candidates_timed_out: Vec<H256>,
	/// A current session index
	current_session_index: u32,
	/// Last finalized block number
	last_finalized_block_number: Option<u32>,
	/// A list of paras for broadcasting
	paras_seen: BTreeMap<u32, u32>,
}

/// Provides collector new head events split by parachain
#[derive(Clone, Debug, Default)]
pub struct NewHeadEvent {
	/// Relay parent block number
	pub relay_parent_number: u32,
	/// Relay parent block hash (or hashes in case of the forks)
	pub relay_parent_hashes: Vec<H256>,
	/// The parachain id (used for broadcasting events)
	pub para_id: u32,
	/// Candidates seen for this relay chain block that belong to the specific `para_id`
	pub candidates_seen: Vec<H256>,
	/// A list of candidate hashes that have been backed in the current block
	pub candidates_backed: Vec<BackedCandidateInfo>,
	/// A list of candidate hashes that have been included in the current block
	pub candidates_included: Vec<H256>,
	/// A list of candidate hashes that have timed out in the current block
	pub candidates_timed_out: Vec<H256>,
	/// Disputes concluded in this block
	pub disputes_concluded: Vec<DisputeInfo>,
}

/// Basic information about a backed candidate
#[derive(Clone, Debug, Default)]
pub struct BackedCandidateInfo {
	pub candidate_hash: H256,
	pub core_idx: u32,
	pub para_id: u32,
}

impl NewHeadEvent {
	pub fn with_relay_parent_number(relay_parent_number: u32) -> Self {
		Self { relay_parent_number, ..Default::default() }
	}
}

/// Handles collector updates
#[derive(Clone, Debug)]
pub enum CollectorUpdateEvent {
	/// Occurs on new block processing with the information about the previous block
	NewHead(NewHeadEvent),
	/// Occurs on a new session
	NewSession(u32),
	/// Occurs when collector is disconnected and is about to terminate
	Termination(TerminationReason),
}

/// Represents the reason for a collector termination
#[derive(Clone, Debug)]
pub enum TerminationReason {
	/// Indicates a normal termination
	Normal,
	/// Indicates an abnormal termination with additional information
	Abnormal(String),
}

#[derive(Debug, Error)]
pub enum CollectorError {
	#[error(transparent)]
	NonFatal(#[from] color_eyre::Report),
	#[error(transparent)]
	ExecutorFatal(#[from] RequestExecutorError),
	#[error(transparent)]
	SendFatal(#[from] polkadot_introspector_priority_channel::SendError),
	#[error("Collector other error: {0}")]
	Other(String),
}

pub struct Collector {
	api: CollectorStorageApi,
	ws_listener: Option<WebSocketListener>,
	to_websocket: Option<Sender<WebSocketUpdateEvent>>,
	endpoint: String,
	subscribe_channels: BTreeMap<u32, Vec<Sender<CollectorUpdateEvent>>>,
	broadcast_channels: Vec<Sender<CollectorUpdateEvent>>,
	state: CollectorState,
	executor: RequestExecutor,
	subscribe_mode: CollectorSubscribeMode,
	hrmp_channels: bool,
	max_parachain_stall: u32,
	hasher: PolkadotHasher,
}

impl Collector {
	pub fn new(endpoint: &str, opts: CollectorOptions, executor: RequestExecutor) -> Self {
		let api: CollectorStorageApi = ApiService::new_with_prefixed_storage(
			RecordsStorageConfig { max_blocks: opts.max_blocks.unwrap_or(64) },
			executor,
		);
		let ws_listener = if let Some(listen_addr) = opts.listen_addr {
			let ws_listener_config = WebSocketListenerConfig::builder().listen_addr(listen_addr).build();
			let ws_listener = WebSocketListener::new(ws_listener_config, api.clone());

			Some(ws_listener)
		} else {
			None
		};
		let executor = api.executor();
		let hasher = executor.hasher(endpoint).expect("hasher should be available");
		Self {
			api,
			ws_listener,
			to_websocket: None,
			endpoint: endpoint.to_owned(),
			subscribe_channels: Default::default(),
			state: Default::default(),
			broadcast_channels: Default::default(),
			executor,
			subscribe_mode: opts.subscribe_mode,
			hrmp_channels: opts.hrmp_channels,
			max_parachain_stall: opts.max_parachain_stall,
			hasher,
		}
	}

	/// Spawns a collector futures (e.g. websocket server)
	pub async fn spawn(&mut self, shutdown_tx: &BroadcastSender<Shutdown>) -> color_eyre::Result<()> {
		if let Some(ws_listener) = &self.ws_listener {
			let (to_websocket, from_collector) = priority_channel(32);
			ws_listener
				.spawn(shutdown_tx.subscribe(), from_collector)
				.await
				.map_err(|e| eyre!("Cannot spawn a listener: {:?}", e))?;
			self.to_websocket = Some(to_websocket);
		}

		Ok(())
	}

	/// Process async channels in the endless loop
	pub async fn run_with_consumer_channel(
		mut self,
		consumer_channel: Receiver<ChainSubscriptionEvent>,
	) -> tokio::task::JoinHandle<()> {
		tokio::spawn(async move {
			let mut consumer_channel = Box::pin(consumer_channel);
			loop {
				match consumer_channel.next().await {
					None => {
						error!("no more events from the consumer channel");
						self.broadcast_event_priority(CollectorUpdateEvent::Termination(TerminationReason::Normal))
							.await
							.unwrap();
						return
					},
					Some(ChainSubscriptionEvent::Termination) => {
						info!("subscribtion terminated");
						self.broadcast_event_priority(CollectorUpdateEvent::Termination(TerminationReason::Normal))
							.await
							.unwrap();
						return
					},
					Some(event) => match self.collect_chain_events(&event).await {
						Ok(subxt_events) =>
							for event in subxt_events.iter() {
								if let Err(error) = self.process_chain_event(event).await {
									error!("collector service could not process event: {:?}", error);
									match error {
										CollectorError::ExecutorFatal(e) => {
											self.broadcast_event_priority(CollectorUpdateEvent::Termination(
												TerminationReason::Abnormal(format!(
													"Collector's executor error: {}",
													e
												)),
											))
											.await
											.unwrap();
											return
										},
										CollectorError::SendFatal(e) => {
											self.broadcast_event_priority(CollectorUpdateEvent::Termination(
												TerminationReason::Abnormal(format!(
													"Collector's channel error: {}",
													e
												)),
											))
											.await
											.unwrap();
											return
										},
										_ => continue,
									}
								}
							},
						Err(e) => {
							error!("collector service could not process events: {:?}", e);
							self.broadcast_event_priority(CollectorUpdateEvent::Termination(
								TerminationReason::Abnormal(format!("Collector's service error: {}", e)),
							))
							.await
							.unwrap();
							return
						},
					},
				}
			}
		})
	}

	/// Collects chain events from new head including block events parsing
	pub async fn collect_chain_events(
		&mut self,
		event: &ChainSubscriptionEvent,
	) -> color_eyre::Result<Vec<ChainEvent<PolkadotConfig>>> {
		let new_head_event = match event {
			ChainSubscriptionEvent::NewBestHead((hash, header)) => ChainEvent::NewBestHead((*hash, header.clone())),
			ChainSubscriptionEvent::NewFinalizedBlock((hash, header)) =>
				ChainEvent::NewFinalizedHead((*hash, header.clone())),
			_ => return Ok(vec![]),
		};
		let mut chain_events = vec![new_head_event];

		if let Some(hash) = new_head_hash(event, self.subscribe_mode) {
			if let Some(block_events) = self.executor.get_events(self.endpoint.as_str(), *hash).await? {
				for block_event in block_events.iter() {
					chain_events.push(decode_chain_event(*hash, block_event.unwrap(), self.hasher).await?);
				}
			}
		};

		Ok(chain_events)
	}

	/// Process a next chain event
	pub async fn process_chain_event<T: subxt::Config>(
		&mut self,
		event: &ChainEvent<T>,
	) -> color_eyre::Result<(), CollectorError> {
		match event {
			ChainEvent::NewBestHead((hash, header)) => self.process_new_best_head(*hash, header).await,
			ChainEvent::NewFinalizedHead((hash, header)) => self.process_new_finalized_head(*hash, header).await,
			ChainEvent::CandidateChanged(change_event) => self.process_candidate_change(change_event).await,
			ChainEvent::DisputeInitiated(dispute_event) => self.process_dispute_initiated(dispute_event).await,
			ChainEvent::DisputeConcluded(dispute_event, dispute_outcome) =>
				self.process_dispute_concluded(dispute_event, dispute_outcome).await,
			ChainEvent::OnDemandOrderPlaced(block_hash, order) =>
				self.process_on_demand_order_placed(block_hash, order).await,
			_ => Ok(()),
		}
	}

	/// Subscribe for parachain updates
	pub async fn subscribe_parachain_updates(
		&mut self,
		para_id: u32,
	) -> color_eyre::Result<Receiver<CollectorUpdateEvent>> {
		let (sender, receiver) = priority_channel_with_capacities(COLLECTOR_NORMAL_CHANNEL_CAPACITY, 1);
		self.subscribe_channels.entry(para_id).or_default().push(sender);

		Ok(receiver)
	}

	/// Subscribe for broadcast updates
	pub async fn subscribe_broadcast_updates(&mut self) -> color_eyre::Result<Receiver<CollectorUpdateEvent>> {
		let (sender, receiver) = priority_channel_with_capacities(COLLECTOR_BROADCAST_CHANNEL_CAPACITY, 1);
		self.broadcast_channels.push(sender);

		Ok(receiver)
	}

	/// Returns API endpoint for storage and request executor
	pub fn api(&self) -> CollectorStorageApi {
		self.api.clone()
	}

	/// Returns RPC executor
	pub fn executor(&self) -> RequestExecutor {
		self.executor.clone()
	}

	async fn update_state(&mut self, block_number: u32, block_hash: H256) -> color_eyre::Result<()> {
		for (para_id, channels) in self.subscribe_channels.iter_mut() {
			let candidates = self.state.candidates_seen.get(para_id);
			let disputes_concluded = self.state.disputes_seen.get(para_id).map(|disputes_seen| {
				disputes_seen
					.iter()
					.filter(|dispute_info| dispute_info.concluded.is_some())
					.cloned()
					.collect::<Vec<_>>()
			});

			for channel in channels {
				channel
					.send(CollectorUpdateEvent::NewHead(NewHeadEvent {
						relay_parent_hashes: self.state.current_relay_chain_block_hashes.clone(),
						relay_parent_number: self.state.current_relay_chain_block_number,
						candidates_seen: candidates.cloned().unwrap_or_default(),
						candidates_backed: self.state.candidates_backed.clone(),
						candidates_included: self.state.candidates_included.clone(),
						candidates_timed_out: self.state.candidates_timed_out.clone(),
						disputes_concluded: disputes_concluded.clone().unwrap_or_default(),
						para_id: *para_id,
					}))
					.await?;
			}
		}

		for broadcast_channel in self.broadcast_channels.iter_mut() {
			// Update a list of current parachains
			for para_id in self.state.candidates_seen.keys() {
				self.state
					.paras_seen
					.insert(*para_id, self.state.current_relay_chain_block_number);
			}
			let to_evict: Vec<_> = self
				.state
				.paras_seen
				.iter()
				.filter_map(|(para_id, last_block)| {
					let is_active =
						self.state.current_relay_chain_block_number - *last_block > self.max_parachain_stall;
					if is_active { Some(*para_id) } else { None }
				})
				.collect();
			for para_id in to_evict {
				let last_seen = self.state.paras_seen.remove(&para_id).expect("checked previously, qed");
				info!(
					"evicting tracker for parachain {}, stalled for {} blocks",
					para_id,
					self.state.current_relay_chain_block_number - last_seen
				);
			}

			for para_id in self.state.paras_seen.keys() {
				let candidates = self.state.candidates_seen.get(para_id);
				let disputes_concluded = self.state.disputes_seen.get(para_id).map(|disputes_seen| {
					disputes_seen
						.iter()
						.filter(|dispute_info| dispute_info.concluded.is_some())
						.cloned()
						.collect::<Vec<_>>()
				});
				broadcast_channel
					.send(CollectorUpdateEvent::NewHead(NewHeadEvent {
						relay_parent_hashes: self.state.current_relay_chain_block_hashes.clone(),
						relay_parent_number: self.state.current_relay_chain_block_number,
						candidates_seen: candidates.cloned().unwrap_or_default(),
						candidates_backed: self.state.candidates_backed.clone(),
						candidates_included: self.state.candidates_included.clone(),
						candidates_timed_out: self.state.candidates_timed_out.clone(),
						disputes_concluded: disputes_concluded.clone().unwrap_or_default(),
						para_id: *para_id,
					}))
					.await?;
			}
		}

		self.state.candidates_seen.clear();
		self.state.candidates_backed.clear();
		self.state.candidates_included.clear();
		self.state.candidates_timed_out.clear();
		self.state.current_relay_chain_block_hashes.clear();
		self.state.current_relay_chain_block_number = block_number;
		self.state.current_relay_chain_block_hashes.push(block_hash);
		Ok(())
	}

	/// Send event to all open channels
	async fn broadcast_event(&mut self, event: CollectorUpdateEvent) -> color_eyre::Result<()> {
		for (_, channels) in self.subscribe_channels.iter_mut() {
			for channel in channels {
				channel.send(event.clone()).await?;
			}
		}

		for broadcast_channel in self.broadcast_channels.iter_mut() {
			broadcast_channel.send(event.clone()).await?;
		}

		Ok(())
	}

	/// Send a priority event to all open channels
	async fn broadcast_event_priority(&mut self, event: CollectorUpdateEvent) -> color_eyre::Result<()> {
		for (_, channels) in self.subscribe_channels.iter_mut() {
			for channel in channels {
				channel.send_priority(event.clone()).await?;
			}
		}

		for broadcast_channel in self.broadcast_channels.iter_mut() {
			broadcast_channel.send_priority(event.clone()).await?;
		}

		Ok(())
	}

	async fn process_new_best_head(
		&mut self,
		block_hash: H256,
		header: &Header,
	) -> color_eyre::Result<(), CollectorError> {
		let ts = self.executor.get_block_timestamp(self.endpoint.as_str(), block_hash).await?;
		let block_number = header.number;

		if self.state.last_finalized_block_number.is_some() {
			self.storage_write_prefixed(
				CollectorPrefixType::RelevantFinalizedBlockNumber,
				block_hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(block_number, Duration::from_secs(ts)),
					self.state.last_finalized_block_number.unwrap(),
				),
			)
			.await?;
		}

		match self.subscribe_mode {
			CollectorSubscribeMode::Best => self.process_new_head(block_hash, header, ts, block_number).await,
			_ => Ok(()),
		}
	}

	async fn process_new_finalized_head(
		&mut self,
		block_hash: H256,
		header: &Header,
	) -> color_eyre::Result<(), CollectorError> {
		let ts = self.executor.get_block_timestamp(self.endpoint.as_str(), block_hash).await?;
		let block_number = header.number;

		if self.state.last_finalized_block_number.is_none() ||
			self.state.last_finalized_block_number.unwrap() < block_number
		{
			self.state.last_finalized_block_number = Some(block_number);
			debug!("Last finalized block {}", block_number);
		}

		match self.subscribe_mode {
			CollectorSubscribeMode::Finalized => self.process_new_head(block_hash, header, ts, block_number).await,
			_ => Ok(()),
		}
	}

	async fn process_new_head(
		&mut self,
		block_hash: H256,
		header: &Header,
		ts: Timestamp,
		block_number: u32,
	) -> color_eyre::Result<(), CollectorError> {
		info!(
			"importing new block hash: {:?}, number: {}, previous number: {}, previous hashes: {:?}",
			block_hash,
			block_number,
			self.state.current_relay_chain_block_number,
			self.state.current_relay_chain_block_hashes
		);

		match block_number.cmp(&self.state.current_relay_chain_block_number) {
			Ordering::Greater => {
				self.write_hrmp_channels(block_hash, block_number, ts).await?;
				self.update_state(block_number, block_hash).await?;
			},
			Ordering::Equal => {
				// A fork
				self.state.current_relay_chain_block_hashes.push(block_hash);
			},
			Ordering::Less => Err(eyre!(
				"Invalid block number: {}, {} is known in the state",
				block_number,
				self.state.current_relay_chain_block_number
			))?,
		}

		self.storage_write_prefixed(
			CollectorPrefixType::RelayBlockHeader,
			block_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(block_number, Duration::from_secs(ts)), header),
		)
		.await?;
		let cur_session = self.executor.get_session_index(self.endpoint.as_str(), block_hash).await?;
		let cur_session_hash = self.hasher.hash(&cur_session.to_be_bytes()[..]);
		let maybe_existing_session = self
			.storage_read_prefixed(CollectorPrefixType::AccountKeys, cur_session_hash)
			.await;
		if maybe_existing_session.is_none() {
			// New session, need to store it's data
			debug!("new session: {}, hash: {}", cur_session, cur_session_hash);
			let accounts_keys = self
				.executor
				.get_session_account_keys(self.endpoint.as_str(), cur_session, None)
				.await?
				.ok_or_else(|| eyre!("Missing account keys for session {}", cur_session))?;
			self.storage_write_prefixed(
				CollectorPrefixType::AccountKeys,
				cur_session_hash,
				StorageEntry::new_persistent(RecordTime::with_ts(block_number, Duration::from_secs(ts)), accounts_keys),
			)
			.await?;
			// Remove old session with the index `cur_session - 2` ignoring possible errors
			if cur_session > 1 {
				let prev_session = cur_session.saturating_sub(2);
				let prev_session_hash = self.hasher.hash(&prev_session.to_be_bytes()[..]);

				let _ = self
					.storage_delete_prefixed(CollectorPrefixType::AccountKeys, prev_session_hash)
					.await;
			}
		}

		if self.state.current_session_index != cur_session {
			self.state.current_session_index = cur_session;
			self.broadcast_event(CollectorUpdateEvent::NewSession(cur_session)).await?;
		}
		self.write_parainherent_data(block_hash, block_number, ts).await?;
		self.write_ts(block_hash, block_number, ts).await?;
		self.write_occupied_cores(block_hash, block_number, ts).await?;
		self.write_backing_groups(block_hash, block_number, ts).await?;
		self.write_core_assignments(block_hash, block_number, ts).await?;

		debug!(
			"Success! new block hash: {:?}, number: {}, previous number: {}, previous hashes: {:?}",
			block_hash,
			block_number,
			self.state.current_relay_chain_block_number,
			self.state.current_relay_chain_block_hashes
		);

		Ok(())
	}

	async fn write_hrmp_channels(
		&mut self,
		block_hash: H256,
		block_number: u32,
		ts: Timestamp,
	) -> color_eyre::Result<(), CollectorError> {
		if !self.hrmp_channels {
			return Ok(())
		}

		let para_ids: Vec<u32> = if self.subscribe_channels.is_empty() {
			self.state.candidates_seen.keys().cloned().collect()
		} else {
			self.subscribe_channels.keys().cloned().collect()
		};

		for (para_id, inbound, outbound) in self
			.executor()
			.get_inbound_outbound_hrmp_channels(&self.endpoint, block_hash, para_ids)
			.await?
		{
			self.storage_write_prefixed(
				CollectorPrefixType::InboundOutboundHrmpChannels(para_id),
				block_hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(block_number, Duration::from_secs(ts)),
					(inbound, outbound),
				),
			)
			.await?;
		}

		Ok(())
	}

	async fn write_parainherent_data(
		&mut self,
		block_hash: H256,
		block_number: u32,
		ts: Timestamp,
	) -> color_eyre::Result<(), CollectorError> {
		let inherent_data = self
			.executor
			.extract_parainherent_data(self.endpoint.as_str(), Some(block_hash))
			.await?;
		self.storage_write_prefixed(
			CollectorPrefixType::InherentData,
			block_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(block_number, Duration::from_secs(ts)), inherent_data),
		)
		.await?;

		Ok(())
	}

	async fn write_ts(
		&self,
		block_hash: H256,
		block_number: u32,
		ts: Timestamp,
	) -> color_eyre::Result<(), CollectorError> {
		self.storage_write_prefixed(
			CollectorPrefixType::Timestamp,
			block_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(block_number, Duration::from_secs(ts)), ts),
		)
		.await?;

		Ok(())
	}

	async fn write_occupied_cores(
		&mut self,
		block_hash: H256,
		block_number: u32,
		ts: Timestamp,
	) -> color_eyre::Result<(), CollectorError> {
		let cores = self.executor.get_occupied_cores(self.endpoint.as_str(), block_hash).await?;
		self.storage_write_prefixed(
			CollectorPrefixType::OccupiedCores,
			block_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(block_number, Duration::from_secs(ts)), cores),
		)
		.await?;

		Ok(())
	}

	async fn write_backing_groups(
		&mut self,
		block_hash: H256,
		block_number: u32,
		ts: Timestamp,
	) -> color_eyre::Result<(), CollectorError> {
		let groups = self.executor.get_backing_groups(self.endpoint.as_str(), block_hash).await?;
		self.storage_write_prefixed(
			CollectorPrefixType::BackingGroups,
			block_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(block_number, Duration::from_secs(ts)), groups),
		)
		.await?;

		Ok(())
	}

	async fn write_core_assignments(
		&mut self,
		block_hash: H256,
		block_number: u32,
		ts: Timestamp,
	) -> color_eyre::Result<(), CollectorError> {
		let assignments = self.core_assignments_via_claim_queue(block_hash).await?;
		self.storage_write_prefixed(
			CollectorPrefixType::CoreAssignments,
			block_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(block_number, Duration::from_secs(ts)), assignments),
		)
		.await?;

		Ok(())
	}

	async fn core_assignments_via_claim_queue(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<ClaimQueue, RequestExecutorError> {
		let assignments = self.executor.get_claim_queue(self.endpoint.as_str(), block_hash).await?;
		Ok(assignments)
	}

	async fn process_candidate_change(
		&mut self,
		change_event: &SubxtCandidateEvent,
	) -> color_eyre::Result<(), CollectorError> {
		match change_event.event_type {
			SubxtCandidateEventType::Backed => {
				// Candidate should not exist in our storage
				if let Some(existing) = self
					.storage_read_prefixed(
						CollectorPrefixType::Candidate(change_event.parachain_id),
						change_event.candidate_hash,
					)
					.await
				{
					// This can happen on forks easily
					let candidate_record: CandidateRecord = existing.into_inner()?;
					info!(
						"duplicate candidate found: {}; relay parent: {}, parachain: {}",
						change_event.candidate_hash,
						candidate_record.candidate_inclusion.relay_parent,
						candidate_record.parachain_id()
					);
					return Ok(());
				}

				let now = get_unix_time_unwrap();
				// Append to the all candidates
				info!(
					"stored candidate backed: {:?}, parachain: {}",
					change_event.candidate_hash, change_event.parachain_id
				);
				self.storage_write_prefixed(
					CollectorPrefixType::CandidatesParachains,
					change_event.candidate_hash,
					StorageEntry::new_onchain(
						RecordTime::with_ts(self.state.current_relay_chain_block_number, now),
						change_event.parachain_id,
					),
				)
				.await?;

				let Some(relay_parent) = self
					.read_or_fetch_header(change_event.candidate_descriptor.relay_parent)
					.await?
				else {
					return Err(eyre!(
						"no stored relay parent {:?} for candidate {:?}, parachain id: {}",
						change_event.candidate_descriptor.relay_parent,
						change_event.candidate_hash,
						change_event.parachain_id
					)
					.into());
				};

				let relay_block_number = self.state.current_relay_chain_block_number;
				let candidate_inclusion = CandidateInclusionRecord {
					relay_parent: change_event.candidate_descriptor.relay_parent,
					relay_parent_number: relay_parent.number,
					parachain_id: change_event.parachain_id,
					backed: relay_block_number,
					core_idx: change_event.core_idx,
					timedout: None,
					included: None,
				};
				let new_record =
					CandidateRecord { candidate_inclusion, candidate_first_seen: now, candidate_disputed: None };
				self.storage_write_prefixed(
					CollectorPrefixType::Candidate(change_event.parachain_id),
					change_event.candidate_hash,
					StorageEntry::new_onchain(RecordTime::with_ts(relay_block_number, now), new_record),
				)
				.await?;
				self.state
					.candidates_seen
					.entry(change_event.parachain_id)
					.or_default()
					.push(change_event.candidate_hash);
				self.state.candidates_backed.push(BackedCandidateInfo {
					candidate_hash: change_event.candidate_hash,
					core_idx: change_event.core_idx,
					para_id: change_event.parachain_id,
				});
				if let Some(to_websocket) = self.to_websocket.as_mut() {
					to_websocket
						.send(WebSocketUpdateEvent {
							candidate_hash: change_event.candidate_hash,
							ts: now,
							parachain_id: change_event.parachain_id,
							event: WebSocketEventType::Backed,
						})
						.await?;
				}
			},
			SubxtCandidateEventType::Included => {
				let maybe_known_candidate = self
					.storage_read_prefixed(
						CollectorPrefixType::Candidate(change_event.parachain_id),
						change_event.candidate_hash,
					)
					.await;

				if let Some(known_candidate) = maybe_known_candidate {
					let now = get_unix_time_unwrap();
					let relay_block_number = self.state.current_relay_chain_block_number;
					let mut known_candidate: CandidateRecord = known_candidate.into_inner()?;
					known_candidate.candidate_inclusion.included = Some(relay_block_number);
					if let Some(to_websocket) = self.to_websocket.as_mut() {
						to_websocket
							.send(WebSocketUpdateEvent {
								candidate_hash: change_event.candidate_hash,
								ts: now,
								parachain_id: change_event.parachain_id,
								event: WebSocketEventType::Included(
									now.saturating_sub(known_candidate.candidate_first_seen),
								),
							})
							.await?;
					}
					self.state
						.candidates_seen
						.entry(change_event.parachain_id)
						.or_default()
						.push(change_event.candidate_hash);
					self.state.candidates_included.push(change_event.candidate_hash);
					self.storage_replace_prefixed(
						CollectorPrefixType::Candidate(change_event.parachain_id),
						change_event.candidate_hash,
						StorageEntry::new_onchain(RecordTime::with_ts(relay_block_number, now), known_candidate),
					)
					.await;
				} else {
					info!(
						"unknown candidate {:?} has been included, parachain: {}",
						change_event.candidate_hash, change_event.parachain_id
					);
				}
			},
			SubxtCandidateEventType::TimedOut => {
				let maybe_known_candidate = self
					.storage_read_prefixed(
						CollectorPrefixType::Candidate(change_event.parachain_id),
						change_event.candidate_hash,
					)
					.await;

				if let Some(known_candidate) = maybe_known_candidate {
					let mut known_candidate: CandidateRecord = known_candidate.into_inner()?;
					let now = get_unix_time_unwrap();
					let relay_block_number = self.state.current_relay_chain_block_number;
					known_candidate.candidate_inclusion.timedout = Some(relay_block_number);
					if let Some(to_websocket) = self.to_websocket.as_mut() {
						to_websocket
							.send(WebSocketUpdateEvent {
								candidate_hash: change_event.candidate_hash,
								ts: now,
								parachain_id: change_event.parachain_id,
								event: WebSocketEventType::TimedOut(
									now.saturating_sub(known_candidate.candidate_first_seen),
								),
							})
							.await?;
					}
					self.state
						.candidates_seen
						.entry(change_event.parachain_id)
						.or_default()
						.push(change_event.candidate_hash);
					self.state.candidates_timed_out.push(change_event.candidate_hash);
					self.storage_replace_prefixed(
						CollectorPrefixType::Candidate(change_event.parachain_id),
						change_event.candidate_hash,
						StorageEntry::new_onchain(RecordTime::with_ts(relay_block_number, now), known_candidate),
					)
					.await;
				} else {
					info!(
						"unknown candidate {:?} has been timed out, parachain: {}",
						change_event.candidate_hash, change_event.parachain_id
					);
				}
			},
		}
		Ok(())
	}

	async fn find_candidate_by_hash(&self, candidate_hash: H256) -> Option<CandidateRecord> {
		let para_id = self
			.storage_read_prefixed(CollectorPrefixType::CandidatesParachains, candidate_hash)
			.await?;
		let para_id: u32 = para_id.into_inner().unwrap();
		let candidate = self
			.storage_read_prefixed(CollectorPrefixType::Candidate(para_id), candidate_hash)
			.await?;
		Some(candidate.into_inner().unwrap())
	}

	async fn process_dispute_initiated(
		&mut self,
		dispute_event: &SubxtDispute,
	) -> color_eyre::Result<(), CollectorError> {
		let mut candidate = self
			.find_candidate_by_hash(dispute_event.candidate_hash)
			.await
			.ok_or_else(|| eyre!("unknown candidate disputed: {:?}", dispute_event.candidate_hash))?;

		let relay_block_number = self.state.current_relay_chain_block_number;
		let now = get_unix_time_unwrap();
		let para_id = candidate.parachain_id();
		candidate.candidate_disputed = Some(CandidateDisputed { disputed: relay_block_number, concluded: None });
		let (initiator_indices, session_index) = self.extract_dispute_initiators(dispute_event).await?;
		if let Some(to_websocket) = self.to_websocket.as_mut() {
			to_websocket
				.send(WebSocketUpdateEvent {
					event: WebSocketEventType::DisputeInitiated(dispute_event.relay_parent_block),
					candidate_hash: dispute_event.candidate_hash,
					ts: now,
					parachain_id: para_id,
				})
				.await?;
		}

		// Fill and write dispute info structure
		let dispute_info = DisputeInfo {
			dispute: dispute_event.clone(),
			initiated: relay_block_number,
			initiator_indices,
			session_index,
			concluded: None,
			parachain_id: candidate.parachain_id(),
			outcome: None,
		};

		self.state
			.disputes_seen
			.entry(candidate.parachain_id())
			.or_default()
			.push(dispute_info.clone());

		self.storage_write_prefixed(
			CollectorPrefixType::Dispute(para_id),
			dispute_event.candidate_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(relay_block_number, now), dispute_info),
		)
		.await?;

		// Update candidate
		self.storage_replace_prefixed(
			CollectorPrefixType::Candidate(para_id),
			dispute_event.candidate_hash,
			StorageEntry::new_onchain(RecordTime::with_ts(relay_block_number, now), candidate),
		)
		.await;

		Ok(())
	}

	async fn extract_dispute_initiators(
		&mut self,
		dispute_event: &SubxtDispute,
	) -> color_eyre::Result<(Vec<u32>, u32)> {
		let default_value = (vec![], self.state.current_session_index);
		let entry = match self
			.storage_read_prefixed(CollectorPrefixType::InherentData, dispute_event.relay_parent_block)
			.await
		{
			Some(v) => v,
			None => return Ok(default_value),
		};

		let data: InherentData = entry.into_inner()?;
		let statement_set = match data
			.disputes
			.iter()
			.find(|&d| d.candidate_hash.0 == dispute_event.candidate_hash)
		{
			Some(v) => v,
			None => return Ok(default_value),
		};

		Ok((
			statement_set
				.statements
				.iter()
				.filter(|(statement, _, _)| matches!(statement, DisputeStatement::Invalid(_)))
				.map(|(_, idx, _)| idx.0)
				.collect(),
			statement_set.session,
		))
	}

	async fn process_dispute_concluded(
		&mut self,
		dispute_event: &SubxtDispute,
		dispute_outcome: &SubxtDisputeResult,
	) -> color_eyre::Result<(), CollectorError> {
		let mut candidate = self
			.find_candidate_by_hash(dispute_event.candidate_hash)
			.await
			.ok_or_else(|| eyre!("unknown candidate dispute concluded: {:?}", dispute_event.candidate_hash))?;
		// TODO: query endpoint for the votes + session keys like pc does
		let now = get_unix_time_unwrap();
		let record_time = RecordTime::with_ts(self.state.current_relay_chain_block_number, now);
		let para_id = candidate.parachain_id();
		candidate.candidate_disputed = Some(CandidateDisputed {
			disputed: self.state.current_relay_chain_block_number,
			concluded: Some(DisputeResult {
				concluded_block: self.state.current_relay_chain_block_number,
				outcome: *dispute_outcome,
			}),
		});
		if let Some(to_websocket) = self.to_websocket.as_mut() {
			to_websocket
				.send(WebSocketUpdateEvent {
					event: WebSocketEventType::DisputeConcluded(dispute_event.relay_parent_block, *dispute_outcome),
					candidate_hash: dispute_event.candidate_hash,
					ts: now,
					parachain_id: candidate.parachain_id(),
				})
				.await?;
		}

		let dispute_info_entry = self
			.storage_read_prefixed(CollectorPrefixType::Dispute(para_id), dispute_event.candidate_hash)
			.await;

		if let Some(dispute_info_entry) = dispute_info_entry {
			let mut dispute_info: DisputeInfo = dispute_info_entry.into_inner()?;
			dispute_info.outcome = Some(*dispute_outcome);
			dispute_info.concluded = Some(self.state.current_relay_chain_block_number);

			self.state
				.disputes_seen
				.entry(candidate.parachain_id())
				.or_default()
				.push(dispute_info.clone());

			self.storage_replace_prefixed(
				CollectorPrefixType::Dispute(para_id),
				dispute_event.candidate_hash,
				StorageEntry::new_onchain(record_time, dispute_info),
			)
			.await;
		} else {
			warn!(
				"dispute for candidate {} is concluded without being seen (parachain id = {})",
				dispute_event.candidate_hash, para_id
			);
		}

		self.storage_replace_prefixed(
			CollectorPrefixType::Candidate(para_id),
			dispute_event.candidate_hash,
			StorageEntry::new_onchain(record_time, candidate),
		)
		.await;
		Ok(())
	}

	async fn process_on_demand_order_placed(
		&self,
		block_hash: &H256,
		order: &OnDemandOrder,
	) -> Result<(), CollectorError> {
		self.storage_write_prefixed(
			CollectorPrefixType::OnDemandOrder(order.para_id),
			*block_hash,
			StorageEntry::new_onchain(
				RecordTime::with_ts(self.state.current_relay_chain_block_number, get_unix_time_unwrap()),
				order,
			),
		)
		.await?;
		Ok(())
	}

	async fn read_or_fetch_header(&self, block_hash: H256) -> Result<Option<Header>, CollectorError> {
		if let Some(storage_entry) = self
			.storage_read_prefixed(CollectorPrefixType::RelayBlockHeader, block_hash)
			.await
		{
			return storage_entry.into_inner::<Header>().map(Some).map_err(|e| e.into())
		}

		if let Some(relay_parent) = self.executor().get_block_head(self.endpoint.as_str(), Some(block_hash)).await? {
			let ts = self.executor().get_block_timestamp(&self.endpoint, block_hash).await?;
			self.storage_write_prefixed(
				CollectorPrefixType::RelayBlockHeader,
				block_hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(relay_parent.number, Duration::from_secs(ts)),
					relay_parent.clone(),
				),
			)
			.await?;

			return Ok(Some(relay_parent))
		}

		Ok(None)
	}

	async fn storage_read_prefixed(&self, p: CollectorPrefixType, k: H256) -> Option<StorageEntry> {
		self.api.storage().storage_read_prefixed(p, k).await
	}

	async fn storage_write_prefixed(&self, p: CollectorPrefixType, k: H256, v: StorageEntry) -> color_eyre::Result<()> {
		self.api.storage().storage_write_prefixed(p, k, v).await
	}

	async fn storage_replace_prefixed(&self, p: CollectorPrefixType, k: H256, v: StorageEntry) {
		self.api.storage().storage_replace_prefixed(p, k, v).await
	}

	async fn storage_delete_prefixed(&self, p: CollectorPrefixType, k: H256) -> Option<StorageEntry> {
		self.api.storage().storage_delete_prefixed(p, k).await
	}
}

fn get_unix_time_unwrap() -> Duration {
	SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

pub fn new_head_hash(event: &ChainSubscriptionEvent, subscribe_mode: CollectorSubscribeMode) -> Option<&H256> {
	match (event, subscribe_mode) {
		(ChainSubscriptionEvent::NewBestHead((hash, _)), CollectorSubscribeMode::Best) => Some(hash),
		(ChainSubscriptionEvent::NewFinalizedBlock((hash, _)), CollectorSubscribeMode::Finalized) => Some(hash),
		_ => None,
	}
}
