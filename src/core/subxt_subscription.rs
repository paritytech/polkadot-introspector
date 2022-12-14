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

use super::{EventConsumerInit, EventStream, MAX_MSG_QUEUE_SIZE, RETRY_DELAY_MS};
use async_trait::async_trait;
use codec::{Decode, Encode};
use color_eyre::eyre::eyre;
use futures::{future, Stream, StreamExt};
use log::{error, info};
use serde::Serialize;
use std::{future::Future, pin::Pin};
use subxt::{
	blocks::Block,
	ext::sp_runtime::traits::{BlakeTwo256, Hash as CryptoHash},
	Error, OnlineClient, PolkadotConfig,
};
use tokio::sync::mpsc::{channel, Sender};

#[cfg(all(feature = "rococo", feature = "polkadot", feature = "versi"))]
compile_error!("`rococo`, `polkadot`, `versi` are mutually exclusive features");

#[cfg(not(any(feature = "rococo", feature = "polkadot", feature = "versi")))]
compile_error!("Must build with either `rococo`, `polkadot`, `versi` features");

#[cfg(feature = "versi")]
#[subxt::subxt(runtime_metadata_path = "assets/versi_metadata_v2.scale")]
pub mod polkadot {}

#[cfg(feature = "rococo")]
#[subxt::subxt(runtime_metadata_path = "assets/rococo_metadata_v2.scale")]
pub mod polkadot {}

#[cfg(feature = "polkadot")]
#[subxt::subxt(runtime_metadata_path = "assets/polkadot_metadata_v2.scale")]
pub mod polkadot {}

use polkadot::{
	para_inclusion::events::{CandidateBacked, CandidateIncluded, CandidateTimedOut},
	paras_disputes::events::{DisputeConcluded, DisputeInitiated, DisputeTimedOut},
	runtime_types::polkadot_primitives::v2::CandidateDescriptor,
};

pub struct SubxtWrapper {
	urls: Vec<String>,
	/// One sender per consumer per URL.
	consumers: Vec<Vec<Sender<SubxtEvent>>>,
	/// Mode of subscription
	subscribe_mode: SubxtSubscriptionMode,
}

/// Dispute result as seen by subxt event
#[derive(Debug, Clone, Copy, Serialize, Decode, Encode)]
pub enum SubxtDisputeResult {
	/// Dispute outcome is valid
	Valid,
	/// Dispute outcome is invalid
	Invalid,
	/// Dispute has been timed out
	TimedOut,
}

/// How to subscribe to subxt blocks
#[derive(strum::Display, Debug, Clone, Copy, clap::ValueEnum)]
pub enum SubxtSubscriptionMode {
	/// Subscribe to all blocks
	All,
	/// Subscribe to the best chain
	Best,
	/// Subscribe to finalized blocks
	Finalized,
}

impl Default for SubxtSubscriptionMode {
	fn default() -> Self {
		SubxtSubscriptionMode::All
	}
}

/// A helper structure to keep track of a dispute and it's relay parent
#[derive(Debug, Clone)]
pub struct SubxtDispute {
	/// Relay chain block where a dispute has taken place
	pub relay_parent_block: <PolkadotConfig as subxt::Config>::Hash,
	/// Specific candidate being disputed about
	pub candidate_hash: <PolkadotConfig as subxt::Config>::Hash,
}

#[derive(Debug)]
pub enum SubxtCandidateEventType {
	/// Candidate has been backed
	Backed,
	/// Candidate has been included
	Included,
	/// Candidate has been timed out
	TimedOut,
}
/// A structure that helps to deal with the candidate events decoding some of
/// the important fields there
#[derive(Debug)]
pub struct SubxtCandidateEvent {
	/// Result of candidate receipt hashing
	pub candidate_hash: <PolkadotConfig as subxt::Config>::Hash,
	/// Full candidate receipt if needed
	pub candidate_descriptor: CandidateDescriptor<<PolkadotConfig as subxt::Config>::Hash>,
	/// The parachain id
	pub parachain_id: u32,
	/// The event type
	pub event_type: SubxtCandidateEventType,
}

#[derive(Debug)]
pub enum SubxtEvent {
	/// New relay chain head
	NewHead(<PolkadotConfig as subxt::Config>::Hash),
	/// Dispute for a specific candidate hash
	DisputeInitiated(SubxtDispute),
	/// Conclusion for a dispute
	DisputeConcluded(SubxtDispute, SubxtDisputeResult),
	/// Backing, inclusion, time out for a parachain candidate
	CandidateChanged(Box<SubxtCandidateEvent>),
	/// Anything undecoded
	RawEvent(<PolkadotConfig as subxt::Config>::Hash, subxt::events::EventDetails),
}

#[async_trait]
impl EventStream for SubxtWrapper {
	type Event = SubxtEvent;

	/// Create a new consumer of events. Returns consumer initialization data.
	fn create_consumer(&mut self) -> EventConsumerInit<Self::Event> {
		let mut update_channels = Vec::new();

		// Create per ws update channels.
		for _ in 0..self.urls.len() {
			update_channels.push(channel(MAX_MSG_QUEUE_SIZE));
		}

		let (update_tx, update_channels) = update_channels.into_iter().unzip();

		// Keep per url update senders for this consumer.
		self.consumers.push(update_tx);

		EventConsumerInit::new(update_channels)
	}

	async fn run(self, tasks: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()> {
		let futures = self
			.consumers
			.into_iter()
			.map(|update_channels| Self::run_per_consumer(update_channels, self.urls.clone(), self.subscribe_mode));

		let mut flat_futures = futures.flatten().collect::<Vec<_>>();
		flat_futures.extend(tasks);
		future::try_join_all(flat_futures).await?;

		Ok(())
	}
}

impl SubxtWrapper {
	pub fn new(urls: Vec<String>, subscribe_mode: SubxtSubscriptionMode) -> SubxtWrapper {
		SubxtWrapper { urls, consumers: Vec::new(), subscribe_mode }
	}

	// Per consumer
	async fn run_per_node(update_channel: Sender<SubxtEvent>, url: String, subscribe_mode: SubxtSubscriptionMode) {
		loop {
			match OnlineClient::<PolkadotConfig>::from_url(url.clone()).await {
				Ok(api) => {
					info!("[{}] Connected", url);
					let ret = match subscribe_mode {
						SubxtSubscriptionMode::All =>
							process_subscription_or_stop(&update_channel, api.blocks().subscribe_all(), url.as_str())
								.await,
						SubxtSubscriptionMode::Best =>
							process_subscription_or_stop(&update_channel, api.blocks().subscribe_best(), url.as_str())
								.await,
						SubxtSubscriptionMode::Finalized =>
							process_subscription_or_stop(
								&update_channel,
								api.blocks().subscribe_finalized(),
								url.as_str(),
							)
							.await,
					};

					if ret {
						// Subscription has decided to stop
						return
					}
				},
				Err(err) => {
					error!("[{}] Disconnected ({:?}) ", url, err);
					// TODO (sometime): Add exponential backoff.
					tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
					info!("[{}] retrying connection ... ", url);
				},
			}
		}
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	fn run_per_consumer(
		update_channels: Vec<Sender<SubxtEvent>>,
		urls: Vec<String>,
		subscribe_mode: SubxtSubscriptionMode,
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls.into_iter())
			.map(|(update_channel, url)| tokio::spawn(Self::run_per_node(update_channel, url, subscribe_mode)))
			.collect()
	}
}

// Subxt does not export this type but it is needed to specify future output
type BlockStream<T> = Pin<Box<dyn Stream<Item = Result<T, Error>> + Send>>;

// Process subscription to a specific block types (e.g. all, best, finalized) and returns `true`
// if a caller's loop should be terminated.
async fn process_subscription_or_stop<Sub, Client>(
	update_channel: &Sender<SubxtEvent>,
	subscription: Sub,
	url: &str,
) -> bool
where
	Sub: Future<Output = Result<BlockStream<Block<PolkadotConfig, Client>>, Error>> + Send + 'static,
	Client: subxt::client::OnlineClientT<PolkadotConfig> + Send + Sync + 'static,
{
	match subscription.await {
		Ok(mut sub) => loop {
			tokio::select! {
				Some(block) = sub.next() => {
					let block = block.unwrap();
					let events = block.events().await.unwrap();
					let hash = block.hash();
					info!("[{}] Block imported ({:?})", url, &hash);

					if let Err(e) = update_channel.send(SubxtEvent::NewHead(hash)).await {
						info!("Event consumer has terminated: {:?}, shutting down", e);
						return true;
					}

					for event in events.iter() {
						let event = event.unwrap();
						decode_or_send_raw_event(hash, event.clone(), update_channel).await.unwrap()
					}
				},
				_ = tokio::signal::ctrl_c() => {
					return true;
				}
			};
		},
		Err(err) => {
			error!("[{}] Disconnected ({:?}) ", url, err);
			// TODO (sometime): Add exponential backoff.
			tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
			info!("[{}] retrying connection ... ", url);
		},
	};

	false
}

async fn decode_or_send_raw_event(
	block_hash: <PolkadotConfig as subxt::Config>::Hash,
	event: subxt::events::EventDetails,
	update_channel: &Sender<SubxtEvent>,
) -> color_eyre::Result<()> {
	use polkadot::runtime_types::polkadot_runtime_parachains::disputes::DisputeResult as RuntimeDisputeResult;

	let subxt_event = if is_specific_event::<DisputeInitiated>(&event) {
		let decoded = decode_to_specific_event::<DisputeInitiated>(&event)?;
		SubxtEvent::DisputeInitiated(SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 })
	} else if is_specific_event::<DisputeConcluded>(&event) {
		let decoded = decode_to_specific_event::<DisputeConcluded>(&event)?;
		let outcome = match decoded.1 {
			RuntimeDisputeResult::Valid => SubxtDisputeResult::Valid,
			RuntimeDisputeResult::Invalid => SubxtDisputeResult::Invalid,
		};
		SubxtEvent::DisputeConcluded(
			SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 },
			outcome,
		)
	} else if is_specific_event::<DisputeTimedOut>(&event) {
		let decoded = decode_to_specific_event::<DisputeTimedOut>(&event)?;
		SubxtEvent::DisputeConcluded(
			SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 },
			SubxtDisputeResult::TimedOut,
		)
	} else if is_specific_event::<CandidateBacked>(&event) {
		let decoded = decode_to_specific_event::<CandidateBacked>(&event)?;
		SubxtEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.descriptor,
			SubxtCandidateEventType::Backed,
		)))
	} else if is_specific_event::<CandidateIncluded>(&event) {
		let decoded = decode_to_specific_event::<CandidateIncluded>(&event)?;
		SubxtEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.descriptor,
			SubxtCandidateEventType::Included,
		)))
	} else if is_specific_event::<CandidateTimedOut>(&event) {
		let decoded = decode_to_specific_event::<CandidateTimedOut>(&event)?;
		SubxtEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.descriptor,
			SubxtCandidateEventType::TimedOut,
		)))
	} else {
		SubxtEvent::RawEvent(block_hash, event)
	};

	update_channel
		.send(subxt_event)
		.await
		.map_err(|e| eyre!("cannot send to the channel: {:?}", e))
}

fn is_specific_event<E: subxt::events::StaticEvent>(raw_event: &subxt::events::EventDetails) -> bool {
	E::is_event(raw_event.pallet_name(), raw_event.variant_name())
}

fn decode_to_specific_event<E: subxt::events::StaticEvent>(
	raw_event: &subxt::events::EventDetails,
) -> color_eyre::Result<E> {
	raw_event
		.as_event()
		.map_err(|e| {
			eyre!(
				"cannot decode event pallet {}, variant {}: {:?}",
				raw_event.pallet_name(),
				raw_event.variant_name(),
				e
			)
		})
		.and_then(|maybe_event| {
			maybe_event.ok_or_else(|| {
				eyre!(
					"cannot decode event pallet {}, variant {}: no event found",
					raw_event.pallet_name(),
					raw_event.variant_name(),
				)
			})
		})
}

fn create_candidate_event(
	candidate_descriptor: CandidateDescriptor<<PolkadotConfig as subxt::Config>::Hash>,
	event_type: SubxtCandidateEventType,
) -> SubxtCandidateEvent {
	let candidate_hash = BlakeTwo256::hash_of(&candidate_descriptor);
	let parachain_id = candidate_descriptor.para_id.0;
	SubxtCandidateEvent { event_type, candidate_descriptor, parachain_id, candidate_hash }
}
