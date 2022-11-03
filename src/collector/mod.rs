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

use clap::Parser;
use codec::Encode;
use log::{debug, info, warn};
use std::{
	default::Default,
	hash::{Hash, Hasher},
	net::SocketAddr,
	time::{Duration, SystemTime, UNIX_EPOCH},
};
use subxt::sp_core::H256;
use tokio::{
	signal,
	sync::{broadcast, mpsc::Receiver},
};

mod candidate_record;
mod ws;

use crate::core::{
	ApiService, EventConsumerInit, HasPrefix, RecordTime, RecordsStorageConfig, StorageEntry, StorageInfo,
	SubxtCandidateEvent, SubxtCandidateEventType, SubxtDispute, SubxtDisputeResult, SubxtEvent,
};
use candidate_record::*;
use color_eyre::eyre::eyre;
use subxt::DefaultConfig;
use tokio::sync::broadcast::Sender;
use ws::*;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct CollectorOptions {
	#[clap(name = "ws", long, value_delimiter = ',', default_value = "wss://westmint-rpc.polkadot.io:443")]
	pub nodes: Vec<String>,
	/// Maximum blocks to store
	#[clap(name = "max-blocks", long)]
	max_blocks: Option<usize>,
	/// WS listen address to bind to
	#[clap(short = 'l', long = "listen", default_value = "0.0.0.0:3030")]
	listen_addr: SocketAddr,
}

impl From<CollectorOptions> for WebSocketListenerConfig {
	fn from(opts: CollectorOptions) -> WebSocketListenerConfig {
		WebSocketListenerConfig::builder().listen_addr(opts.listen_addr).build()
	}
}

/// This type is used to distinguish different keys in the storage
#[derive(Clone, Copy, Debug, Hash, Ord, PartialOrd, Eq, PartialEq)]
pub(crate) enum CollectorPrefixType {
	/// Candidate prefixed by ParachainId
	Candidate(Option<u32>),
	/// Relay chain block
	Head,
}

/// Used to query storage for multiple hash types
#[derive(Clone, Debug, Ord, PartialOrd, Eq)]
pub(crate) struct CollectorKey {
	/// Prefix for a specific collector key
	pub prefix: CollectorPrefixType,
	/// Block or a candidate hash
	pub hash: H256,
}

impl CollectorKey {
	pub(crate) fn new_relay_chain_block(hash: H256) -> Self {
		Self { prefix: CollectorPrefixType::Head, hash }
	}
	pub(crate) fn new_parachain_candidate(hash: H256, para_id: u32) -> Self {
		Self { prefix: CollectorPrefixType::Candidate(Some(para_id)), hash }
	}
	// This is used when we don't know the parachain id
	pub(crate) fn new_generic_hash(hash: H256) -> Self {
		Self { prefix: CollectorPrefixType::Candidate(None), hash }
	}

	pub(crate) fn new_with_prefix(prefix: CollectorPrefixType) -> Self {
		Self { prefix, hash: H256::zero() }
	}
}

impl HasPrefix for CollectorKey {
	fn has_prefix(&self, other: &Self) -> bool {
		match self.prefix {
			CollectorPrefixType::Head => other.prefix == self.prefix,
			CollectorPrefixType::Candidate(maybe_para) => match other.prefix {
				CollectorPrefixType::Head => false,
				CollectorPrefixType::Candidate(maybe_other_para) =>
					maybe_other_para.is_none() || maybe_para == maybe_other_para,
			},
		}
	}
}

// This implementation compares the hashes and optionally checks for the prefix
// For example, when we have no parachain id available, we should be able to find
// a specific entry with no hash
impl PartialEq for CollectorKey {
	fn eq(&self, other: &Self) -> bool {
		self.hash == other.hash && self.has_prefix(other)
	}
}

impl Hash for CollectorKey {
	fn hash<H: Hasher>(&self, state: &mut H) {
		state.write(self.hash.as_bytes())
	}
}

pub(crate) async fn run(
	opts: CollectorOptions,
	consumer_config: EventConsumerInit<SubxtEvent>,
) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
	let api_service =
		ApiService::new_with_storage(RecordsStorageConfig { max_blocks: opts.max_blocks.unwrap_or(1000) });
	let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
	let (to_websocket, _) = broadcast::channel(32);
	let endpoints = opts.nodes.clone().into_iter();

	let consumer_channels: Vec<Receiver<SubxtEvent>> = consumer_config.into();
	let ws_listener = WebSocketListener::new(opts.clone().into(), api_service.clone());
	ws_listener
		.spawn(shutdown_rx, to_websocket.clone())
		.await
		.map_err(|e| eyre!("Cannot spawn a listener: {:?}", e))?;

	let mut futures = endpoints
		.zip(consumer_channels.into_iter())
		.map(|(endpoint, mut update_channel)| {
			let mut shutdown_rx = shutdown_tx.subscribe();
			let to_websocket = to_websocket.clone();
			let api_service = api_service.clone();

			tokio::spawn(async move {
				loop {
					debug!("[{}] New loop - waiting for events", endpoint);

					tokio::select! {
						Some(event) = update_channel.recv() => {
							debug!("New event: {:?}", event);
							let result = match event {
								SubxtEvent::NewHead(block_hash) => {
									process_new_head(endpoint.as_str(), &api_service, block_hash).await
								},
								SubxtEvent::CandidateChanged(change_event) => {
									process_candidate_change(&api_service, *change_event, &to_websocket).await
								},
								SubxtEvent::DisputeInitiated(dispute_event) => {
									process_dispute_initiated(&api_service, dispute_event, &to_websocket).await
								},
								SubxtEvent::DisputeConcluded(dispute_event, dispute_outcome) => {
									process_dispute_concluded(&api_service, dispute_event, dispute_outcome, &to_websocket).await
								},
								_ => Ok(()),
							};
							match result {
								Ok(_) => continue,
								Err(e) => {
									warn!("error processing event: {:?}", e);
								}
							}
						},
						_ = shutdown_rx.recv() => {
							info!("shutting down");
							break;
						}
					}
				}
			})
		})
		.collect::<Vec<_>>();

	futures.push(tokio::spawn(async move {
		signal::ctrl_c().await.unwrap();
		let _ = shutdown_tx.send(());
	}));
	Ok(futures)
}

async fn process_new_head(
	url: &str,
	api_service: &ApiService<CollectorKey>,
	block_hash: H256,
) -> color_eyre::Result<()> {
	let executor = api_service.subxt();
	let ts = executor.get_block_timestamp(url.into(), Some(block_hash)).await;
	let header = executor
		.get_block_head(url.into(), Some(block_hash))
		.await
		.ok_or_else(|| eyre!("Missing block {}", block_hash))?;
	api_service
		.storage()
		.storage_write(
			CollectorKey::new_relay_chain_block(block_hash),
			StorageEntry::new_onchain(RecordTime::with_ts(header.number, Duration::from_secs(ts)), header.encode()),
		)
		.await
		.unwrap();
	Ok(())
}

async fn process_candidate_change(
	api_service: &ApiService<CollectorKey>,
	change_event: SubxtCandidateEvent,
	to_websocket: &Sender<WebSocketUpdateEvent>,
) -> color_eyre::Result<()> {
	let storage = api_service.storage();
	match change_event.event_type {
		SubxtCandidateEventType::Backed => {
			// Candidate should not exist in our storage
			let maybe_existing = storage
				.storage_read(CollectorKey::new_parachain_candidate(
					change_event.candidate_hash,
					change_event.parachain_id,
				))
				.await;
			if let Some(existing) = maybe_existing {
				let candidate_descriptor: CandidateRecord = existing.into_inner()?;
				info!(
					"duplicate candidate found: {}, parachain: {}",
					change_event.candidate_hash,
					candidate_descriptor.parachain_id()
				);
			} else {
				// Find the relay parent
				let maybe_relay_parent = storage
					.storage_read(CollectorKey::new_relay_chain_block(change_event.candidate_descriptor.relay_parent))
					.await;

				if let Some(relay_parent) = maybe_relay_parent {
					let now = get_unix_time_unwrap();
					let parent: <DefaultConfig as subxt::Config>::Header = relay_parent.into_inner()?;
					let block_number = parent.number;
					let candidate_inclusion = CandidateInclusion {
						relay_parent: change_event.candidate_descriptor.relay_parent,
						parachain_id: change_event.parachain_id,
						backed: now,
						core_idx: None,
						timedout: None,
						included: None,
					};
					let new_record = CandidateRecord {
						candidate_inclusion,
						candidate_first_seen: now,
						candidate_descriptor: change_event.candidate_descriptor,
						candidate_disputed: None,
					};
					api_service
						.storage()
						.storage_write(
							CollectorKey::new_parachain_candidate(
								change_event.candidate_hash,
								change_event.parachain_id,
							),
							StorageEntry::new_onchain(
								RecordTime::with_ts(block_number, Duration::from_secs(now.as_secs())),
								new_record.encode(),
							),
						)
						.await
						.unwrap();
					to_websocket
						.send(WebSocketUpdateEvent {
							candidate_hash: change_event.candidate_hash,
							ts: now,
							parachain_id: change_event.parachain_id,
							event: WebSocketEventType::Backed,
						})
						.unwrap();
				} else {
					return Err(eyre!(
						"no stored relay parent {} for candidate {}",
						change_event.candidate_descriptor.relay_parent,
						change_event.candidate_hash
					))
				}
			}
		},
		SubxtCandidateEventType::Included => {
			let maybe_known_candidate = api_service
				.storage()
				.storage_read(CollectorKey::new_parachain_candidate(
					change_event.candidate_hash,
					change_event.parachain_id,
				))
				.await;

			if let Some(known_candidate) = maybe_known_candidate {
				let record_time = known_candidate.time();
				let mut known_candidate: CandidateRecord = known_candidate.into_inner()?;
				let now = get_unix_time_unwrap();
				known_candidate.candidate_inclusion.included = Some(now);
				to_websocket
					.send(WebSocketUpdateEvent {
						candidate_hash: change_event.candidate_hash,
						ts: now,
						parachain_id: change_event.parachain_id,
						event: WebSocketEventType::Included(now.saturating_sub(known_candidate.candidate_first_seen)),
					})
					.unwrap();
				api_service
					.storage()
					.storage_replace(
						CollectorKey::new_parachain_candidate(change_event.candidate_hash, change_event.parachain_id),
						StorageEntry::new_onchain(record_time, known_candidate.encode()),
					)
					.await;
			} else {
				info!("unknown candidate {} has been included", change_event.candidate_hash);
			}
		},
		SubxtCandidateEventType::TimedOut => {
			let maybe_known_candidate = api_service
				.storage()
				.storage_read(CollectorKey::new_parachain_candidate(
					change_event.candidate_hash,
					change_event.parachain_id,
				))
				.await;

			if let Some(known_candidate) = maybe_known_candidate {
				let record_time = known_candidate.time();
				let mut known_candidate: CandidateRecord = known_candidate.into_inner()?;
				let now = get_unix_time_unwrap();
				known_candidate.candidate_inclusion.timedout = Some(now);
				to_websocket
					.send(WebSocketUpdateEvent {
						candidate_hash: change_event.candidate_hash,
						ts: now,
						parachain_id: change_event.parachain_id,
						event: WebSocketEventType::TimedOut(now.saturating_sub(known_candidate.candidate_first_seen)),
					})
					.unwrap();
				api_service
					.storage()
					.storage_replace(
						CollectorKey::new_parachain_candidate(change_event.candidate_hash, change_event.parachain_id),
						StorageEntry::new_onchain(record_time, known_candidate.encode()),
					)
					.await;
			} else {
				info!("unknown candidate {} has been timed out", change_event.candidate_hash);
			}
		},
	}
	Ok(())
}

async fn process_dispute_initiated(
	api_service: &ApiService<CollectorKey>,
	dispute_event: SubxtDispute,
	to_websocket: &Sender<WebSocketUpdateEvent>,
) -> color_eyre::Result<()> {
	let candidate = api_service
		.storage()
		.storage_read(CollectorKey::new_generic_hash(dispute_event.candidate_hash))
		.await
		.ok_or_else(|| eyre!("unknown candidate disputed"))?;
	let record_time = candidate.time();
	let mut candidate: CandidateRecord = candidate.into_inner()?;
	let now = get_unix_time_unwrap();
	candidate.candidate_disputed = Some(CandidateDisputed { disputed: now, concluded: None });
	to_websocket.send(WebSocketUpdateEvent {
		event: WebSocketEventType::DisputeInitiated(dispute_event.relay_parent_block),
		candidate_hash: dispute_event.candidate_hash,
		ts: now,
		parachain_id: candidate.parachain_id(),
	})?;
	api_service
		.storage()
		.storage_replace(
			CollectorKey::new_generic_hash(dispute_event.candidate_hash),
			StorageEntry::new_onchain(record_time, candidate.encode()),
		)
		.await;
	Ok(())
}

async fn process_dispute_concluded(
	api_service: &ApiService<CollectorKey>,
	dispute_event: SubxtDispute,
	dispute_outcome: SubxtDisputeResult,
	to_websocket: &Sender<WebSocketUpdateEvent>,
) -> color_eyre::Result<()> {
	let candidate = api_service
		.storage()
		.storage_read(CollectorKey::new_generic_hash(dispute_event.candidate_hash))
		.await
		.ok_or_else(|| eyre!("unknown candidate disputed"))?;
	// TODO: query enpoint for the votes + session keys like pc does
	let record_time = candidate.time();
	let mut candidate: CandidateRecord = candidate.into_inner()?;
	let now = get_unix_time_unwrap();
	candidate.candidate_disputed = Some(CandidateDisputed {
		disputed: now,
		concluded: Some(DisputeResult { concluded_timestamp: now, outcome: dispute_outcome }),
	});
	to_websocket.send(WebSocketUpdateEvent {
		event: WebSocketEventType::DisputeConcluded(dispute_event.relay_parent_block, dispute_outcome),
		candidate_hash: dispute_event.candidate_hash,
		ts: now,
		parachain_id: candidate.parachain_id(),
	})?;
	api_service
		.storage()
		.storage_replace(
			CollectorKey::new_generic_hash(dispute_event.candidate_hash),
			StorageEntry::new_onchain(record_time, candidate.encode()),
		)
		.await;
	Ok(())
}

fn get_unix_time_unwrap() -> Duration {
	SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}
