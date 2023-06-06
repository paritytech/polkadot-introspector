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
		subxt_wrapper::{RequestExecutor, SubxtWrapperError},
		ApiService,
	},
	chain_events::{
		decode_chain_event, ChainEvent, SubxtCandidateEvent, SubxtCandidateEventType, SubxtDispute, SubxtDisputeResult,
	},
	chain_head_subscription::ChainHeadEvent,
	storage::{RecordTime, RecordsStorageConfig, StorageEntry},
	types::{Timestamp, H256},
	utils::RetryOptions,
};
use candidate_record::{CandidateDisputed, CandidateInclusionRecord, CandidateRecord, DisputeResult};
use clap::{Parser, ValueEnum};
use codec::{Decode, Encode};
use color_eyre::eyre::eyre;
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use polkadot_introspector_priority_channel::{
	channel as priority_channel, channel_with_capacities as priority_channel_with_capacities, Receiver, Sender,
};
use std::{
	cmp::Ordering,
	collections::BTreeMap,
	default::Default,
	hash::Hash,
	net::SocketAddr,
	time::{Duration, SystemTime, UNIX_EPOCH},
};
use subxt::{
	config::{
		substrate::{BlakeTwo256, SubstrateHeader},
		Hasher,
	},
	PolkadotConfig,
};
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
	#[clap(short = 's', long = "subscribe-mode", default_value_t, value_enum)]
	pub subscribe_mode: CollectorSubscribeMode,
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
	/// Inherent data (more expensive to store, so good to have it shared)
	InherentData,
	/// Dispute information indexed by Parachain-Id; data is DisputeInfo
	Dispute(u32),
}

/// A type that defines prefix + hash itself
pub type CollectorStorageApi = ApiService<H256, CollectorPrefixType>;

/// A structure used to track disputes progress
#[derive(Clone, Debug, Encode, Decode)]
pub struct DisputeInfo {
	pub initiated: <PolkadotConfig as subxt::Config>::Index,
	pub dispute: SubxtDispute,
	pub parachain_id: u32,
	pub outcome: Option<SubxtDisputeResult>,
	pub concluded: Option<<PolkadotConfig as subxt::Config>::Index>,
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
	/// A current session index
	current_session_index: u32,
	/// Last finalized block number
	last_finalized_block_number: Option<u32>,
}

/// Provides collector new head events split by parachain
#[derive(Clone, Debug)]
pub struct NewHeadEvent {
	/// Relay parent block number
	pub relay_parent_number: <PolkadotConfig as subxt::Config>::Index,
	/// Relay parent block hash (or hashes in case of the forks)
	pub relay_parent_hashes: Vec<H256>,
	/// The parachain id (used for broadcasting events)
	pub para_id: u32,
	/// Candidates seen for this relay chain block that belong to the specific `para_id`
	pub candidates_seen: Vec<H256>,
	/// Disputes concluded in this block
	pub disputes_concluded: Vec<DisputeInfo>,
}

/// Handles collector updates
#[derive(Clone, Debug)]
pub enum CollectorUpdateEvent {
	/// Occurs on new block processing with the information about the previous block
	NewHead(NewHeadEvent),
	/// Occurs on a new session
	NewSession(u32),
	/// Occurs when collector is disconnected and is about to terminate
	Termination,
}

#[derive(Debug, Error)]
pub enum CollectorError {
	#[error(transparent)]
	NonFatal(#[from] color_eyre::Report),
	#[error(transparent)]
	ExecutorFatal(#[from] SubxtWrapperError),
	#[error(transparent)]
	SendFatal(#[from] polkadot_introspector_priority_channel::SendError),
}

pub struct Collector {
	api_service: CollectorStorageApi,
	ws_listener: Option<WebSocketListener>,
	to_websocket: Option<Sender<WebSocketUpdateEvent>>,
	endpoint: String,
	subscribe_channels: BTreeMap<u32, Vec<Sender<CollectorUpdateEvent>>>,
	broadcast_channels: Vec<Sender<CollectorUpdateEvent>>,
	state: CollectorState,
	executor: RequestExecutor,
	subscribe_mode: CollectorSubscribeMode,
}

impl Collector {
	pub fn new(endpoint: &str, opts: CollectorOptions, retry: RetryOptions) -> Self {
		let api_service: CollectorStorageApi = ApiService::new_with_prefixed_storage(
			RecordsStorageConfig { max_blocks: opts.max_blocks.unwrap_or(64) },
			retry,
		);
		let ws_listener = if let Some(listen_addr) = opts.listen_addr {
			let ws_listener_config = WebSocketListenerConfig::builder().listen_addr(listen_addr).build();
			let ws_listener = WebSocketListener::new(ws_listener_config, api_service.clone());

			Some(ws_listener)
		} else {
			None
		};
		let executor = api_service.subxt();
		Self {
			api_service,
			ws_listener,
			to_websocket: None,
			endpoint: endpoint.to_owned(),
			subscribe_channels: Default::default(),
			state: Default::default(),
			broadcast_channels: Default::default(),
			executor,
			subscribe_mode: opts.subscribe_mode,
		}
	}

	/// Spawns a collector futures (e.g. websocket server)
	pub async fn spawn(&mut self, shutdown_tx: &BroadcastSender<()>) -> color_eyre::Result<()> {
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
		mut consumer_channel: Receiver<ChainHeadEvent>,
	) -> tokio::task::JoinHandle<()> {
		tokio::spawn(async move {
			loop {
				match consumer_channel.next().await {
					Some(event) => match self.collect_chain_events(&event).await {
						Ok(subxt_events) =>
							for event in subxt_events.iter() {
								if let Err(error) = self.process_chain_event(event).await {
									error!("collector service could not process event: {}", error);
									match error {
										CollectorError::ExecutorFatal(_) | CollectorError::SendFatal(_) => {
											self.broadcast_event_priority(CollectorUpdateEvent::Termination)
												.await
												.unwrap();
											return
										},
										_ => continue,
									}
								}
							},
						Err(e) => {
							error!("collector service could not process events: {}", e);
							self.broadcast_event_priority(CollectorUpdateEvent::Termination).await.unwrap();
							return
						},
					},
					None => {
						error!("no more events from the consumer channel");
						self.broadcast_event_priority(CollectorUpdateEvent::Termination).await.unwrap();
						return
					},
				}
			}
		})
	}

	/// Collects chain events from new head including block events parsing
	pub async fn collect_chain_events(
		&mut self,
		event: &ChainHeadEvent,
	) -> color_eyre::Result<Vec<ChainEvent<PolkadotConfig>>> {
		let new_head_event = match event {
			ChainHeadEvent::NewBestHead(hash) => ChainEvent::NewBestHead(*hash),
			ChainHeadEvent::NewFinalizedHead(hash) => ChainEvent::NewFinalizedHead(*hash),
			ChainHeadEvent::Heartbeat => return Ok(vec![]),
		};
		let mut chain_events = vec![new_head_event];

		if let Some(hash) = new_head_hash(event, self.subscribe_mode) {
			if let Some(block_events) = self.executor.get_events(self.endpoint.as_str(), *hash).await? {
				for block_event in block_events.iter() {
					chain_events.push(decode_chain_event(*hash, block_event.unwrap()).await?);
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
			ChainEvent::NewBestHead(block_hash) => self.process_new_best_head(*block_hash).await,
			ChainEvent::NewFinalizedHead(block_hash) => self.process_new_finalized_head(*block_hash).await,
			ChainEvent::CandidateChanged(change_event) => self.process_candidate_change(change_event).await,
			ChainEvent::DisputeInitiated(dispute_event) => self.process_dispute_initiated(dispute_event).await,
			ChainEvent::DisputeConcluded(dispute_event, dispute_outcome) =>
				self.process_dispute_concluded(dispute_event, dispute_outcome).await,
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
		self.api_service.clone()
	}

	/// Returns Subxt request executor
	pub fn executor(&self) -> RequestExecutor {
		self.executor.clone()
	}

	async fn update_state(
		&mut self,
		block_number: <PolkadotConfig as subxt::Config>::Index,
		block_hash: H256,
	) -> color_eyre::Result<()> {
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
						disputes_concluded: disputes_concluded.clone().unwrap_or_default(),
						para_id: *para_id,
					}))
					.await?;
			}
		}

		for broadcast_channel in self.broadcast_channels.iter_mut() {
			for (para_id, candidates) in self.state.candidates_seen.iter() {
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
						candidates_seen: candidates.clone(),
						disputes_concluded: disputes_concluded.clone().unwrap_or_default(),
						para_id: *para_id,
					}))
					.await?;
			}
		}

		self.state.candidates_seen.clear();
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
	async fn get_head_details(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<(SubstrateHeader<u32, BlakeTwo256>, u64, u32), CollectorError> {
		let header = self
			.executor
			.get_block_head(self.endpoint.as_str(), Some(block_hash))
			.await?
			.ok_or_else(|| eyre!("Missing block {}", block_hash))?;
		let ts = self.executor.get_block_timestamp(self.endpoint.as_str(), block_hash).await?;
		let block_number = header.number;

		Ok((header, ts, block_number))
	}

	async fn process_new_best_head(&mut self, block_hash: H256) -> color_eyre::Result<(), CollectorError> {
		let (header, ts, block_number) = self.get_head_details(block_hash).await?;

		if self.state.last_finalized_block_number.is_some() {
			self.api_service
				.storage()
				.storage_write_prefixed(
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

	async fn process_new_finalized_head(&mut self, block_hash: H256) -> color_eyre::Result<(), CollectorError> {
		let (header, ts, block_number) = self.get_head_details(block_hash).await?;

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
		header: SubstrateHeader<u32, BlakeTwo256>,
		ts: Timestamp,
		block_number: u32,
	) -> color_eyre::Result<(), CollectorError> {
		info!(
			"imported new block hash: {:?}, number: {}, previous number: {}, previous hashes: {:?}",
			block_hash,
			block_number,
			self.state.current_relay_chain_block_number,
			self.state.current_relay_chain_block_hashes
		);

		match block_number.cmp(&self.state.current_relay_chain_block_number) {
			Ordering::Greater => {
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

		self.api_service
			.storage()
			.storage_write_prefixed(
				CollectorPrefixType::RelayBlockHeader,
				block_hash,
				StorageEntry::new_onchain(RecordTime::with_ts(block_number, Duration::from_secs(ts)), header),
			)
			.await
			.unwrap();
		let cur_session = self.executor.get_session_index(self.endpoint.as_str(), block_hash).await?;
		let cur_session_hash = BlakeTwo256::hash(&cur_session.to_be_bytes()[..]);
		let maybe_existing_session = self
			.api_service
			.storage()
			.storage_read_prefixed(CollectorPrefixType::AccountKeys, cur_session_hash)
			.await;
		if maybe_existing_session.is_none() {
			// New session, need to store it's data
			debug!("new session: {}, hash: {}", cur_session, cur_session_hash);
			let accounts_keys = self
				.executor
				.get_session_account_keys(self.endpoint.as_str(), cur_session)
				.await?
				.ok_or_else(|| eyre!("Missing account keys for session {}", cur_session))?;
			self.api_service
				.storage()
				.storage_write_prefixed(
					CollectorPrefixType::AccountKeys,
					cur_session_hash,
					StorageEntry::new_persistent(
						RecordTime::with_ts(block_number, Duration::from_secs(ts)),
						accounts_keys,
					),
				)
				.await?;
			// Remove old session with the index `cur_session - 2` ignoring possible errors
			if cur_session > 1 {
				let prev_session = cur_session.saturating_sub(2);
				let prev_session_hash = BlakeTwo256::hash(&prev_session.to_be_bytes()[..]);

				let _ = self
					.api_service
					.storage()
					.storage_delete_prefixed(CollectorPrefixType::AccountKeys, prev_session_hash)
					.await;
			}
		}

		if self.state.current_session_index != cur_session {
			self.state.current_session_index = cur_session;
			self.broadcast_event(CollectorUpdateEvent::NewSession(cur_session)).await?;
		}

		let inherent_data = self
			.executor
			.extract_parainherent_data(self.endpoint.as_str(), Some(block_hash))
			.await?;

		if let Some(inherent_data) = inherent_data {
			self.api_service
				.storage()
				.storage_write_prefixed(
					CollectorPrefixType::InherentData,
					block_hash,
					StorageEntry::new_onchain(
						RecordTime::with_ts(block_number, Duration::from_secs(ts)),
						inherent_data,
					),
				)
				.await?;
		} else {
			warn!("cannot get inherent data for block number {} ({})", block_number, block_hash);
		}

		Ok(())
	}

	async fn process_candidate_change(
		&mut self,
		change_event: &SubxtCandidateEvent,
	) -> color_eyre::Result<(), CollectorError> {
		let storage = self.api_service.storage();
		match change_event.event_type {
			SubxtCandidateEventType::Backed => {
				// Candidate should not exist in our storage
				let maybe_existing = storage
					.storage_read_prefixed(
						CollectorPrefixType::Candidate(change_event.parachain_id),
						change_event.candidate_hash,
					)
					.await;
				if let Some(existing) = maybe_existing {
					// This can happen on forks easily
					let candidate_record: CandidateRecord = existing.into_inner()?;
					info!(
						"duplicate candidate found: {}; relay parent: {}, parachain: {}",
						change_event.candidate_hash,
						candidate_record.candidate_inclusion.relay_parent,
						candidate_record.parachain_id()
					);
				} else {
					let now = get_unix_time_unwrap();
					// Append to the all candidates
					info!(
						"stored candidate backed: {:?}, parachain: {}",
						change_event.candidate_hash, change_event.parachain_id
					);
					storage
						.storage_write_prefixed(
							CollectorPrefixType::CandidatesParachains,
							change_event.candidate_hash,
							StorageEntry::new_onchain(
								RecordTime::with_ts(self.state.current_relay_chain_block_number, now),
								change_event.parachain_id,
							),
						)
						.await?;
					// Find the relay parent
					let maybe_relay_parent = storage
						.storage_read_prefixed(
							CollectorPrefixType::RelayBlockHeader,
							change_event.candidate_descriptor.relay_parent,
						)
						.await;

					if let Some(_relay_parent) = maybe_relay_parent {
						let relay_block_number = self.state.current_relay_chain_block_number;
						let candidate_inclusion = CandidateInclusionRecord {
							relay_parent: change_event.candidate_descriptor.relay_parent,
							parachain_id: change_event.parachain_id,
							backed: relay_block_number,
							core_idx: None,
							timedout: None,
							included: None,
						};
						let new_record = CandidateRecord {
							candidate_inclusion,
							candidate_first_seen: now,
							candidate_disputed: None,
						};
						self.api_service
							.storage()
							.storage_write_prefixed(
								CollectorPrefixType::Candidate(change_event.parachain_id),
								change_event.candidate_hash,
								StorageEntry::new_onchain(RecordTime::with_ts(relay_block_number, now), new_record),
							)
							.await
							.unwrap();
						self.state
							.candidates_seen
							.entry(change_event.parachain_id)
							.or_default()
							.push(change_event.candidate_hash);
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
					} else {
						Err(eyre!(
							"no stored relay parent {:?} for candidate {:?}, parachain id: {}",
							change_event.candidate_descriptor.relay_parent,
							change_event.candidate_hash,
							change_event.parachain_id
						))?
					}
				}
			},
			SubxtCandidateEventType::Included => {
				let maybe_known_candidate = self
					.api_service
					.storage()
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
					self.api_service
						.storage()
						.storage_replace_prefixed(
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
					.api_service
					.storage()
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
					self.api_service
						.storage()
						.storage_replace_prefixed(
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
			.api_service
			.storage()
			.storage_read_prefixed(CollectorPrefixType::CandidatesParachains, candidate_hash)
			.await?;
		let para_id: u32 = para_id.into_inner().unwrap();
		let candidate = self
			.api_service
			.storage()
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
			initiated: self.state.current_relay_chain_block_number,
			concluded: None,
			parachain_id: candidate.parachain_id(),
			outcome: None,
		};

		self.state
			.disputes_seen
			.entry(candidate.parachain_id())
			.or_default()
			.push(dispute_info.clone());

		self.api_service
			.storage()
			.storage_write_prefixed(
				CollectorPrefixType::Dispute(para_id),
				dispute_event.candidate_hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(self.state.current_relay_chain_block_number, now),
					dispute_info,
				),
			)
			.await?;

		// Update candidate
		self.api_service
			.storage()
			.storage_replace_prefixed(
				CollectorPrefixType::Candidate(para_id),
				dispute_event.candidate_hash,
				StorageEntry::new_onchain(
					RecordTime::with_ts(self.state.current_relay_chain_block_number, now),
					candidate,
				),
			)
			.await;

		Ok(())
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
			.api_service
			.storage()
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

			self.api_service
				.storage()
				.storage_replace_prefixed(
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

		self.api_service
			.storage()
			.storage_replace_prefixed(
				CollectorPrefixType::Candidate(para_id),
				dispute_event.candidate_hash,
				StorageEntry::new_onchain(record_time, candidate),
			)
			.await;
		Ok(())
	}
}

fn get_unix_time_unwrap() -> Duration {
	SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

pub fn new_head_hash(event: &ChainHeadEvent, subscribe_mode: CollectorSubscribeMode) -> Option<&H256> {
	match (event, subscribe_mode) {
		(ChainHeadEvent::NewBestHead(hash), CollectorSubscribeMode::Best) => Some(hash),
		(ChainHeadEvent::NewFinalizedHead(hash), CollectorSubscribeMode::Finalized) => Some(hash),
		_ => None,
	}
}
