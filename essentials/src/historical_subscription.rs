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
	init::Shutdown,
	types::BlockNumber,
};
use async_trait::async_trait;
use log::{debug, error, info};
use polkadot_introspector_priority_channel::{channel, Sender};
use tokio::{
	sync::broadcast::{error::TryRecvError, Sender as BroadcastSender},
	time::{interval_at, Duration},
};

pub struct HistoricalSubscription {
	urls: Vec<String>,
	from_block_number: BlockNumber,
	to_block_number: BlockNumber,
	/// One sender per consumer per URL.
	consumers: Vec<Vec<Sender<ChainSubscriptionEvent>>>,
	executor: RequestExecutor,
}

#[async_trait]
impl EventStream for HistoricalSubscription {
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

	async fn run(
		&self,
		shutdown_tx: &BroadcastSender<Shutdown>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let futures = self.consumers.clone().into_iter().map(|update_channels| {
			Self::run_per_consumer(
				update_channels,
				self.urls.clone(),
				self.from_block_number,
				self.to_block_number,
				shutdown_tx.clone(),
				&self.executor,
			)
		});

		Ok(futures.flatten().collect::<Vec<_>>())
	}
}

impl HistoricalSubscription {
	pub fn new(
		urls: Vec<String>,
		from_block_number: BlockNumber,
		to_block_number: BlockNumber,
		executor: RequestExecutor,
	) -> HistoricalSubscription {
		Self { urls, from_block_number, to_block_number, consumers: Vec::new(), executor }
	}

	// Per node
	async fn run_per_node(
		mut update_channel: Sender<ChainSubscriptionEvent>,
		url: String, // `String` rather than `&str` because we spawn this method as an asynchronous task
		from_block_number: BlockNumber,
		to_block_number: BlockNumber,
		shutdown_tx: BroadcastSender<Shutdown>,
		mut executor: RequestExecutor,
	) {
		let mut shutdown_rx = shutdown_tx.subscribe();
		const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);
		let mut heartbeat_periodic = interval_at(tokio::time::Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);

		let last_block_number = match executor.get_block_number(&url, None).await {
			Ok(v) => v,
			Err(_) => {
				error!("Subscription to {} failed, last block not found", url);
				return
			},
		};

		if from_block_number >= last_block_number || to_block_number >= last_block_number {
			error!("`--from` and `--to` must be less then {}", last_block_number);
			return
		}

		for block_number in from_block_number..=to_block_number {
			debug!("Subscription advacing to block #{}/#{}", block_number, to_block_number);

			let block_hash = match executor.get_block_hash(&url, Some(block_number)).await {
				Ok(Some(v)) => v,
				Ok(None) => {
					error!("Subscription to {} failed, block hash for block #{} not found", url, block_number);
					let _ = shutdown_tx.send(Shutdown::Restart);
					return
				},
				Err(e) => {
					error!("Subscription to {} failed: {:?}", url, e);
					let _ = shutdown_tx.send(Shutdown::Restart);
					return
				},
			};
			let header = match executor.get_block_head(&url, Some(block_hash)).await {
				Ok(Some(v)) => v,
				Ok(None) => {
					error!("Subscription to {} failed, header for block #{} not found", url, block_number);
					let _ = shutdown_tx.send(Shutdown::Restart);
					return
				},
				Err(e) => {
					error!("Subscription to {} failed: {:?}", url, e);
					let _ = shutdown_tx.send(Shutdown::Restart);
					return
				},
			};

			info!("[{}] Block imported ({:?})", url, block_hash);
			if let Err(e) = update_channel
				.send(ChainSubscriptionEvent::NewBestHead((block_hash, header.clone())))
				.await
			{
				info!("Event consumer has terminated: {:?}, shutting down", e);
				return
			}
			if let Err(e) = update_channel
				.send(ChainSubscriptionEvent::NewFinalizedBlock((block_hash, header)))
				.await
			{
				info!("Event consumer has terminated: {:?}, shutting down", e);
				return
			}

			match shutdown_rx.try_recv() {
				Err(TryRecvError::Closed) | Ok(_) => {
					info!("Received interrupt signal shutting down subscription");
					return
				},
				_ => {},
			}
		}

		loop {
			// We wait here for termination.
			tokio::select! {
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
					heartbeat_periodic = interval_at(tokio::time::Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
				}
			}
		}
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	fn run_per_consumer(
		update_channels: Vec<Sender<ChainSubscriptionEvent>>,
		urls: Vec<String>,
		from_block_number: BlockNumber,
		to_block_number: BlockNumber,
		shutdown_tx: BroadcastSender<Shutdown>,
		executor: &RequestExecutor,
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls)
			.map(|(update_channel, url)| {
				tokio::spawn(Self::run_per_node(
					update_channel,
					url,
					from_block_number,
					to_block_number,
					shutdown_tx.clone(),
					executor.clone(),
				))
			})
			.collect()
	}
}
