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
//

use crate::{
	api::subxt_wrapper::RequestExecutor,
	chain_subscription::ChainSubscriptionEvent,
	constants::MAX_MSG_QUEUE_SIZE,
	consumer::{EventConsumerInit, EventStream},
	utils::RetryOptions,
};
use async_trait::async_trait;
use log::{debug, error, info};
use polkadot_introspector_priority_channel::{channel, Sender};
use subxt::rpc::types::FollowEvent;
use tokio::{
	sync::broadcast::Sender as BroadcastSender,
	time::{interval_at, Duration},
};

pub struct ChainHeadSubscription {
	urls: Vec<String>,
	/// One sender per consumer per URL.
	consumers: Vec<Vec<Sender<ChainSubscriptionEvent>>>,
	retry: RetryOptions,
}

#[async_trait]
impl EventStream for ChainHeadSubscription {
	type Event = ChainSubscriptionEvent;

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

	async fn run(self, shutdown_tx: &BroadcastSender<()>) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let futures = self.consumers.into_iter().map(|update_channels| {
			Self::run_per_consumer(update_channels, self.urls.clone(), shutdown_tx.clone(), self.retry.clone())
		});

		Ok(futures.flatten().collect::<Vec<_>>())
	}
}

impl ChainHeadSubscription {
	pub fn new(urls: Vec<String>, retry: RetryOptions) -> ChainHeadSubscription {
		ChainHeadSubscription { urls, consumers: Vec::new(), retry }
	}

	// Per node
	async fn run_per_node(
		mut update_channel: Sender<ChainSubscriptionEvent>,
		url: String, // `String` rather than `&str` because we spawn this method as an asynchronous task
		shutdown_tx: BroadcastSender<()>,
		retry: RetryOptions,
	) {
		let mut shutdown_rx = shutdown_tx.subscribe();
		let mut executor = RequestExecutor::new(retry);
		let (mut sub, sub_id) = match executor.get_chain_head_subscription(&url).await {
			Ok(v) => v,
			Err(e) => {
				error!("Subscription to {} failed: {:?}", url, e);
				std::process::exit(1)
			},
		};

		const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(200);
		let mut heartbeat_periodic = interval_at(tokio::time::Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);

		loop {
			tokio::select! {
				message = sub.next() => {
					let event = match message {
						Some(Ok(v)) => v,
						Some(Err(e)) => {
							error!("Subscription to {} failed: {:?}", url, e);
							std::process::exit(1)
						},
						None => {
							error!("Subscription to {} failed, received None instead of an event", url);
							std::process::exit(1);
						}
					};

					match event {
						// Drain the initialized event
						FollowEvent::Initialized(init) => {
							if let Err(e) = executor.unpin_chain_head(&url, &sub_id, init.finalized_block_hash).await {
								error!("Cannot unpin hash {}: {:?}", init.finalized_block_hash, e);
							};
						},
						FollowEvent::NewBlock(_) => continue,
						FollowEvent::BestBlockChanged(best_block) => {
							info!("[{}] Best block imported ({:?})", url, best_block.best_block_hash);
							if let Err(e) = update_channel.send(ChainSubscriptionEvent::NewBestHead(best_block.best_block_hash)).await {
								info!("Event consumer has terminated: {:?}, shutting down", e);
								return;
							}
						},
						FollowEvent::Finalized(finalized) => {
							for hash in finalized.finalized_block_hashes.iter() {
								info!("[{}] Finalized block imported ({:?})", url, hash);
								if let Err(e) = update_channel.send(ChainSubscriptionEvent::NewFinalizedBlock(*hash)).await {
									info!("Event consumer has terminated: {:?}, shutting down", e);
									return;
								}
							}

							for hash in finalized
								.finalized_block_hashes
								.iter()
								.chain(finalized.pruned_block_hashes.iter())
							{
								if let Err(e) = executor.unpin_chain_head(&url, &sub_id, *hash).await {
									error!("Cannot unpin hash {}: {:?}", hash, e);
								};
							}
						},
						FollowEvent::Stop => {
							info!("Chain head subscription stopped");
							return;
						},
					}
				},
				_ = shutdown_rx.recv() => {
					info!("Received interrupt signal shutting down subscription");
					return;
				}
				_ = heartbeat_periodic.tick() => {
					debug!("sent heartbeat to subscribers");
					let res = update_channel.send(ChainSubscriptionEvent::Heartbeat).await;
					if let Err(e) = res {
						info!("Event consumer has terminated: {:?}, shutting down", e);
						return;
					}
				}
			}
		}
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	fn run_per_consumer(
		update_channels: Vec<Sender<ChainSubscriptionEvent>>,
		urls: Vec<String>,
		shutdown_tx: BroadcastSender<()>,
		retry: RetryOptions,
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls.into_iter())
			.map(|(update_channel, url)| {
				tokio::spawn(Self::run_per_node(update_channel, url, shutdown_tx.clone(), retry.clone()))
			})
			.collect()
	}
}
