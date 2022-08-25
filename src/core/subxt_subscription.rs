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
// TODO: rename to subxt subscription.

use async_trait::async_trait;
use color_eyre::eyre::eyre;
use futures::{future, StreamExt};
use log::{error, info};
use subxt::{ClientBuilder, DefaultConfig, PolkadotExtrinsicParams};

#[subxt::subxt(runtime_metadata_path = "assets/polkadot_metadata_v2.scale")]
pub mod polkadot {}

use tokio::sync::mpsc::{channel, Sender};

use super::{EventConsumerInit, EventStream, MAX_MSG_QUEUE_SIZE, RETRY_DELAY_MS};

pub struct SubxtWrapper {
	urls: Vec<String>,
	/// One sender per consumer per url.
	consumers: Vec<Vec<Sender<SubxtEvent>>>,
}

/// Dispute result as seen by subxt event
#[derive(Debug)]
pub enum SubxtDisputeResult {
	/// Dispute outcome is valid
	Valid,
	/// Dispute outcome is invalid
	Invalid,
	/// Dispute has been timed out
	TimedOut,
}

/// A helper structure to keep track of a dispute and it's relay parent
#[derive(Debug, Clone)]
pub struct SubxtDispute {
	relay_parent_block: <DefaultConfig as subxt::Config>::Hash,
	candidate_hash: <DefaultConfig as subxt::Config>::Hash,
}

#[derive(Debug)]
pub enum SubxtEvent {
	/// New relay chain head
	NewHead(<DefaultConfig as subxt::Config>::Hash),
	/// Dispute for a specific candidate hash
	DisputeInitiated(SubxtDispute),
	/// Conclusion for a dispute
	DisputeConcluded(SubxtDispute, SubxtDisputeResult),
	/// Anything undecoded
	RawEvent(<DefaultConfig as subxt::Config>::Hash, subxt::RawEventDetails),
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
			.map(|update_channels| Self::run_per_consumer(update_channels, self.urls.clone()));

		let mut flat_futures = futures.flatten().collect::<Vec<_>>();
		flat_futures.extend(tasks);
		future::try_join_all(flat_futures).await?;

		Ok(())
	}
}

impl SubxtWrapper {
	pub fn new(urls: Vec<String>) -> SubxtWrapper {
		SubxtWrapper { urls, consumers: Vec::new() }
	}

	// Per consumer
	async fn run_per_node(update_channel: Sender<SubxtEvent>, url: String) {
		loop {
			match ClientBuilder::new().set_url(url.clone()).build().await {
				Ok(api) => {
					let api = api
						.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, PolkadotExtrinsicParams<DefaultConfig>>>(
						);
					info!("[{}] Connected", url);
					match api.events().subscribe_finalized().await {
						Ok(mut sub) => loop {
							tokio::select! {
								Some(events) = sub.next() => {
									let events = events.unwrap();
									let hash = events.block_hash();
									info!("[{}] Block imported ({:?})", url, &hash);

									if let Err(e) = update_channel.send(SubxtEvent::NewHead(hash)).await {
										info!("Event consumer has terminated: {:?}, shutting down", e);
										return;
									}

									for event in events.iter_raw() {
										let event = event.unwrap();
										decode_or_send_raw_event(hash.clone(), event.clone(), &update_channel).await.unwrap()
									}
								},
								_ = tokio::signal::ctrl_c() => {
									return;
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
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls.into_iter())
			.map(|(update_channel, url)| tokio::spawn(Self::run_per_node(update_channel, url)))
			.collect()
	}
}

async fn decode_or_send_raw_event(
	block_hash: <DefaultConfig as subxt::Config>::Hash,
	event: subxt::events::RawEventDetails,
	update_channel: &Sender<SubxtEvent>,
) -> color_eyre::Result<()> {
	use polkadot::runtime_types::polkadot_runtime_parachains::disputes::DisputeResult as RuntimeDisputeResult;
	let event_pallet = event.pallet.as_str();
	let event_variant = event.variant.as_str();

	let subxt_event = match event_pallet {
		"ParasDisputes" => match event_variant {
			"DisputeInitiated" => {
				let decoded = <polkadot::paras_disputes::events::DisputeInitiated as codec::Decode>::decode(
					&mut &event.bytes[..],
				)?;
				SubxtEvent::DisputeInitiated(SubxtDispute {
					relay_parent_block: block_hash,
					candidate_hash: decoded.0 .0,
				})
			},
			"DisputeConcluded" => {
				let decoded = <polkadot::paras_disputes::events::DisputeConcluded as codec::Decode>::decode(
					&mut &event.bytes[..],
				)?;
				let outcome = match decoded.1 {
					RuntimeDisputeResult::Valid => SubxtDisputeResult::Valid,
					RuntimeDisputeResult::Invalid => SubxtDisputeResult::Invalid,
				};
				SubxtEvent::DisputeConcluded(
					SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 },
					outcome,
				)
			},
			"DisputeTimedOut" => {
				let decoded = <polkadot::paras_disputes::events::DisputeTimedOut as codec::Decode>::decode(
					&mut &event.bytes[..],
				)?;
				SubxtEvent::DisputeConcluded(
					SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 },
					SubxtDisputeResult::TimedOut,
				)
			},
			_ => SubxtEvent::RawEvent(block_hash, event),
		},
		_ => SubxtEvent::RawEvent(block_hash, event),
	};

	update_channel
		.send(subxt_event)
		.await
		.map_err(|e| eyre!("cannot send to the channel: {:?}", e))
}
