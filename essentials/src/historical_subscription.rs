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
	init::RunContext,
	types::BlockNumber,
};
use async_trait::async_trait;
use log::{debug, error, info};
use polkadot_introspector_priority_channel::{Sender, channel};

macro_rules! send_termination {
	($ch:expr) => {
		if let Err(e) = $ch.send(ChainSubscriptionEvent::Termination).await {
			info!("Event consumer has already terminated: {:?}", e);
		}
	};
}

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

	async fn run(&self, run_context: &RunContext) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let futures = self.consumers.clone().into_iter().map(|update_channels| {
			Self::run_per_consumer(
				update_channels,
				self.urls.clone(),
				self.from_block_number,
				self.to_block_number,
				run_context.clone(),
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
		url: String,
		from_block_number: BlockNumber,
		to_block_number: BlockNumber,
		run_context: RunContext,
		mut executor: RequestExecutor,
	) {
		let cancel_rx = run_context.subscribe_cancel();

		let last_block_number = match executor.get_block_number(&url, None).await {
			Ok(v) => v,
			Err(e) => {
				error!("Subscription to {} failed to fetch last block: {:?}", url, e);
				send_termination!(update_channel);
				run_context.request_restart();
				return
			},
		};

		if from_block_number >= last_block_number || to_block_number >= last_block_number {
			error!("`--from` and `--to` must be less then {}", last_block_number);
			send_termination!(update_channel);
			run_context.complete();
			return
		}

		for block_number in from_block_number..=to_block_number {
			if *cancel_rx.borrow() {
				info!("Received interrupt signal shutting down subscription");
				send_termination!(update_channel);
				return;
			}

			debug!("Subscription advacing to block #{}/#{}", block_number, to_block_number);

			let block_hash = match executor.get_block_hash(&url, Some(block_number)).await {
				Ok(Some(v)) => v,
				Ok(None) => {
					error!("Subscription to {} failed, block hash for block #{} not found", url, block_number);
					send_termination!(update_channel);
					run_context.request_restart();
					return
				},
				Err(e) => {
					error!("Subscription to {} failed: {:?}", url, e);
					send_termination!(update_channel);
					run_context.request_restart();
					return
				},
			};
			let header = match executor.get_block_head(&url, Some(block_hash)).await {
				Ok(Some(v)) => v,
				Ok(None) => {
					error!("Subscription to {} failed, header for block #{} not found", url, block_number);
					send_termination!(update_channel);
					run_context.request_restart();
					return
				},
				Err(e) => {
					error!("Subscription to {} failed: {:?}", url, e);
					send_termination!(update_channel);
					run_context.request_restart();
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
		}

		info!("[{}] Historical range completed, terminating subscription", url);
		send_termination!(update_channel);
		run_context.complete();
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	fn run_per_consumer(
		update_channels: Vec<Sender<ChainSubscriptionEvent>>,
		urls: Vec<String>,
		from_block_number: BlockNumber,
		to_block_number: BlockNumber,
		run_context: RunContext,
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
					run_context.clone(),
					executor.clone(),
				))
			})
			.collect()
	}
}
