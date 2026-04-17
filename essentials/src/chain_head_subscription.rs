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
	api::executor::RequestExecutor,
	chain_subscription::ChainSubscriptionEvent,
	constants::MAX_MSG_QUEUE_SIZE,
	consumer::{EventConsumerInit, EventStream},
};
use async_trait::async_trait;
use log::{debug, error, info};
use polkadot_introspector_priority_channel::{Sender, channel};
use tokio::{
	sync::broadcast::Sender as BroadcastSender,
	time::{Duration, interval_at},
};

pub struct ChainHeadSubscription {
	urls: Vec<String>,
	/// One sender per consumer per URL.
	consumers: Vec<Vec<Sender<ChainSubscriptionEvent>>>,
	executor: RequestExecutor,
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

	async fn run(&self, shutdown_tx: &BroadcastSender<()>) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let futures = self.consumers.clone().into_iter().map(|update_channels| {
			Self::run_per_consumer(update_channels, self.urls.clone(), shutdown_tx.clone(), &self.executor)
		});

		Ok(futures.flatten().collect::<Vec<_>>())
	}
}

impl ChainHeadSubscription {
	pub fn new(urls: Vec<String>, executor: RequestExecutor) -> ChainHeadSubscription {
		ChainHeadSubscription { urls, consumers: Vec::new(), executor }
	}

	/// Check for absence of new blocks within a timeout.
	/// If none, consider the connection to the RPC stalled
	/// and return `true`.
	fn check_stall(url: &str, last_block_at: tokio::time::Instant) -> bool {
		const BLOCK_STALL_TIMEOUT: Duration = Duration::from_secs(120);
		if last_block_at.elapsed() > BLOCK_STALL_TIMEOUT {
			error!(
				"No new blocks from {} for {:?} — RPC connection appears stalled",
				url,
				last_block_at.elapsed()
			);
			return true;
		}
		false
	}

	// Per node
	async fn run_per_node(
		mut update_channel: Sender<ChainSubscriptionEvent>,
		url: String, // `String` rather than `&str` because we spawn this method as an asynchronous task
		shutdown_tx: BroadcastSender<()>,
		mut executor: RequestExecutor,
	) {
		use futures::stream::{StreamExt, select};
		use futures_util::TryStreamExt;

		let mut shutdown_rx = shutdown_tx.subscribe();
		let best_sub = match executor.get_best_block_subscription(&url).await {
			Ok(v) => v.map_ok(|v| ChainSubscriptionEvent::NewBestHead((v.1.hash(), v.0))),
			Err(e) => {
				error!("Subscription to {} failed: {:?}", url, e);
				let _ = update_channel.send(ChainSubscriptionEvent::Termination).await;
				let _ = shutdown_tx.send(());
				return
			},
		};
		let finalized_sub = match executor.get_finalized_block_subscription(&url).await {
			Ok(v) => v.map_ok(|v| ChainSubscriptionEvent::NewFinalizedBlock((v.1.hash(), v.0))),
			Err(e) => {
				error!("Subscription to {} failed: {:?}", url, e);
				let _ = update_channel.send(ChainSubscriptionEvent::Termination).await;
				let _ = shutdown_tx.send(());
				return
			},
		};
		let mut sub = select(best_sub, finalized_sub);

		const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(200);
		let mut heartbeat_periodic = interval_at(tokio::time::Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
		let mut last_block_at = tokio::time::Instant::now();

		loop {
			tokio::select! {
				message = sub.next() => {
					let event = match message {
						Some(Ok(v)) => v,
						Some(Err(e)) => {
							error!("Subscription to {} failed: {:?}", url, e);
							let _ = update_channel.send(ChainSubscriptionEvent::Termination).await;
							let _ = shutdown_tx.send(());
							return
						},
						None => {
							error!("Subscription to {} failed, received None instead of an event", url);
							let _ = update_channel.send(ChainSubscriptionEvent::Termination).await;
							let _ = shutdown_tx.send(());
							return
						}
					};

					last_block_at = tokio::time::Instant::now();

					if let Err(e) = update_channel.send(event).await {
						info!("Event consumer has terminated: {:?}, shutting down", e);
						return;
					}
				},
				_ = shutdown_rx.recv() => {
					info!("Received interrupt signal shutting down subscription");
					if let Err(e) = update_channel.send(ChainSubscriptionEvent::Termination).await {
						info!("Event consumer has already terminated: {:?}", e);
					}
					return;
				}
				_ = heartbeat_periodic.tick() => {
					if Self::check_stall(&url, last_block_at) {
						// Terminate only this stalled task, shutdown broadcast would kill all
						let _ = update_channel.send(ChainSubscriptionEvent::Termination).await;
						return;
					}
					debug!("sent heartbeat to subscribers");
					if let Err(e) = update_channel.send(ChainSubscriptionEvent::Heartbeat).await {
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
		executor: &RequestExecutor,
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls)
			.map(|(update_channel, url)| {
				tokio::spawn(Self::run_per_node(update_channel, url, shutdown_tx.clone(), executor.clone()))
			})
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn check_stall_returns_false_when_recent() {
		let last_block_at = tokio::time::Instant::now();
		assert!(!ChainHeadSubscription::check_stall("wss://test:443", last_block_at));
	}

	#[test]
	fn check_stall_returns_true_when_exceeded() {
		let last_block_at = tokio::time::Instant::now() - Duration::from_secs(121);
		assert!(ChainHeadSubscription::check_stall("wss://test:443", last_block_at));
	}

	#[test]
	fn check_stall_returns_false_at_boundary() {
		let last_block_at = tokio::time::Instant::now() - Duration::from_secs(119);
		assert!(!ChainHeadSubscription::check_stall("wss://test:443", last_block_at));
	}
}
