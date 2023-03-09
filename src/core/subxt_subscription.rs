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

use super::{EventConsumerInit, EventStream, SubxtEvent, MAX_MSG_QUEUE_SIZE, RETRY_DELAY_MS};
use async_trait::async_trait;
use futures::{future, Stream, StreamExt};
use log::{error, info};
use std::{future::Future, pin::Pin};
use subxt::{blocks::Block, Error, OnlineClient, PolkadotConfig};
use tokio::sync::{
	broadcast::{Receiver as BroadcastReceiver, Sender as BroadcastSender},
	mpsc::{channel, Sender},
};

pub struct SubxtWrapper {
	urls: Vec<String>,
	/// One sender per consumer per URL.
	consumers: Vec<Vec<Sender<SubxtEvent>>>,
	/// Mode of subscription
	subscribe_mode: SubxtSubscriptionMode,
}

/// How to subscribe to subxt blocks
#[derive(strum::Display, Debug, Clone, Copy, clap::ValueEnum, Default)]
pub enum SubxtSubscriptionMode {
	/// Subscribe to the best chain
	Best,
	/// Subscribe to finalized blocks
	#[default]
	Finalized,
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

	async fn run(
		self,
		tasks: Vec<tokio::task::JoinHandle<()>>,
		shutdown_tx: BroadcastSender<()>,
	) -> color_eyre::Result<()> {
		let futures = self.consumers.into_iter().map(|update_channels| {
			Self::run_per_consumer(update_channels, self.urls.clone(), self.subscribe_mode, shutdown_tx.clone())
		});

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
	async fn run_per_node(
		update_channel: Sender<SubxtEvent>,
		url: String,
		subscribe_mode: SubxtSubscriptionMode,
		shutdown_tx: BroadcastSender<()>,
	) {
		let mut shutdown_rx = shutdown_tx.subscribe();
		loop {
			tokio::select! {
				client = OnlineClient::<PolkadotConfig>::from_url(url.clone()) => {
					match client {
						Ok(api) => {
							info!("[{}] Connected", url);
							match subscribe_mode {
								SubxtSubscriptionMode::Best =>
									process_subscription_or_stop(&update_channel, api.blocks().subscribe_best(), url.as_str(), shutdown_tx.subscribe())
										.await,
								SubxtSubscriptionMode::Finalized =>
									process_subscription_or_stop(
										&update_channel,
										api.blocks().subscribe_finalized(),
										url.as_str(),
										shutdown_tx.subscribe()
									)
										.await,
							};
							break;
						},
						Err(err) => {
							error!("[{}] Disconnected ({:?}) ", url, err);
							// TODO (sometime): Add exponential backoff.
							tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
							info!("[{}] retrying connection ... ", url);
						},
					}
				}
				_ = shutdown_rx.recv() => {
					info!("received interrupt signal shutting down subscription");
					return;
				}
			}
		}
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	fn run_per_consumer(
		update_channels: Vec<Sender<SubxtEvent>>,
		urls: Vec<String>,
		subscribe_mode: SubxtSubscriptionMode,
		shutdown_tx: BroadcastSender<()>,
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls.into_iter())
			.map(|(update_channel, url)| {
				tokio::spawn(Self::run_per_node(update_channel, url, subscribe_mode, shutdown_tx.clone()))
			})
			.collect()
	}
}

// Subxt does not export this type but it is needed to specify future output
type BlockStream<T> = Pin<Box<dyn Stream<Item = Result<T, Error>> + Send>>;

// Process subscription to a specific block types (e.g. best, finalized) and returns `true`
// if a caller's loop should be terminated.
async fn process_subscription_or_stop<Sub, Client>(
	update_channel: &Sender<SubxtEvent>,
	subscription: Sub,
	url: &str,
	mut shutdown_rx: BroadcastReceiver<()>,
) where
	Sub: Future<Output = Result<BlockStream<Block<PolkadotConfig, Client>>, Error>> + Send + 'static,
	Client: subxt::client::OnlineClientT<PolkadotConfig> + Send + Sync + 'static,
{
	loop {
		tokio::select! {
			Ok(mut sub) = subscription => loop {
				tokio::select! {
					Some(block) = sub.next() => {
						let block = block.unwrap();
						let hash = block.hash();
						info!("[{}] Block imported ({:?})", url, &hash);

						if let Err(e) = update_channel.send(SubxtEvent::NewHead(hash)).await {
							info!("Event consumer has terminated: {:?}, shutting down", e);
							return;
						}
					},
					_ = shutdown_rx.recv() => {
						info!("received interrupt signal shutting down subscription");
						return;
					}
				}
			},
			_ = shutdown_rx.recv() => {
				info!("received interrupt signal shutting down subscription");
				return;
			}
		}
	}
}
