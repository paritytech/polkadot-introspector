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
	constants::MAX_MSG_QUEUE_SIZE,
	consumer::{EventConsumerInit, EventStream},
	types::H256,
	utils::{Retry, RetryOptions},
};
use async_trait::async_trait;
use futures::future;
use log::{error, info};
use polkadot_introspector_priority_channel::{channel, SendError, Sender};
use subxt::{
	rpc::{types::FollowEvent, Subscription},
	OnlineClient, PolkadotConfig,
};
use tokio::sync::broadcast::{Receiver as BroadcastReceiver, Sender as BroadcastSender};

#[derive(Debug)]
pub enum SubxtEvent {
	/// New relay chain best head
	NewBestHead(<PolkadotConfig as subxt::Config>::Hash),
	/// New relay chain finalized head
	NewFinalizedHead(<PolkadotConfig as subxt::Config>::Hash),
}

pub struct SubxtSubscription {
	urls: Vec<String>,
	/// One sender per consumer per URL.
	consumers: Vec<Vec<Sender<SubxtEvent>>>,
	retry: RetryOptions,
}

#[async_trait]
impl EventStream for SubxtSubscription {
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
			Self::run_per_consumer(update_channels, self.urls.clone(), shutdown_tx.clone(), self.retry.clone())
		});

		let mut flat_futures = futures.flatten().collect::<Vec<_>>();
		flat_futures.extend(tasks);
		future::try_join_all(flat_futures).await?;

		Ok(())
	}
}

impl SubxtSubscription {
	pub fn new(urls: Vec<String>, retry: RetryOptions) -> SubxtSubscription {
		SubxtSubscription { urls, consumers: Vec::new(), retry }
	}

	// Per consumer
	async fn run_per_node(
		mut update_channel: Sender<SubxtEvent>,
		url: String,
		shutdown_tx: BroadcastSender<()>,
		retry: RetryOptions,
	) {
		if let Some(api) = subxt_client(url.clone(), shutdown_tx.subscribe(), &retry).await {
			let mut shutdown_rx = shutdown_tx.subscribe();
			let (mut sub, sub_id) = subxt_chain_head_subscription(&api).await;

			loop {
				tokio::select! {
					Some(Ok(event)) = sub.next() => {
						match event {
							// Drain the initialized event
							FollowEvent::Initialized(init) => {
								subxt_unpin_hash(&api, sub_id.clone(), init.finalized_block_hash).await;
							},
							FollowEvent::NewBlock(_) => continue,
							FollowEvent::BestBlockChanged(best_block) => {
								info!("[{}] Best block imported ({:?})", url, best_block.best_block_hash);
								if let Err(e) = update_channel.send(SubxtEvent::NewBestHead(best_block.best_block_hash)).await {
									return on_error(e);
								}
							},
							FollowEvent::Finalized(finalized) => {
								for hash in finalized.finalized_block_hashes.iter() {
									info!("[{}] Finalized block imported ({:?})", url, hash);
									if let Err(e) = update_channel.send(SubxtEvent::NewFinalizedHead(*hash)).await {
										return on_error(e);
									}
								}

								for hash in finalized
									.finalized_block_hashes
									.iter()
									.chain(finalized.pruned_block_hashes.iter())
								{
									subxt_unpin_hash(&api, sub_id.clone(), *hash).await;
								}
							},
							FollowEvent::Stop => {
								on_subscription_stop();
								break;
							},
						}
					},
					_ = shutdown_rx.recv() => {
						return on_ctrl_c();
					}

				}
			}
		}
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	fn run_per_consumer(
		update_channels: Vec<Sender<SubxtEvent>>,
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

async fn subxt_client(
	url: String,
	mut shutdown_rx: BroadcastReceiver<()>,
	retry: &RetryOptions,
) -> Option<OnlineClient<PolkadotConfig>> {
	let mut retry = Retry::new(retry);
	loop {
		tokio::select! {
			client = OnlineClient::<PolkadotConfig>::from_url(url.clone()) => {
				match client {
					Ok(api) => {
						info!("[{}] Connected", url);
						return Some(api)
					},
					Err(err) => {
						error!("[subxt_client, {}] Disconnected, will attempt to retry ({:?})", url, err);
						if (retry.sleep().await).is_err() {
							error!("[subxt_client, {}] The attempt limit has been reached", url);
							return None
						}
					},
				}
			}
			_ = shutdown_rx.recv() => {
				on_ctrl_c();
				return None;
			}
		}
	}
}

async fn subxt_chain_head_subscription(
	api: &OnlineClient<PolkadotConfig>,
) -> (Subscription<FollowEvent<H256>>, String) {
	match api.rpc().chainhead_unstable_follow(true).await {
		Ok(sub) => {
			let sub_id = sub.subscription_id().expect("A subscription ID must be provided").to_string();
			(sub, sub_id)
		},
		Err(err) => panic!("Cannot subscribe to head updates: {:?}", err),
	}
}

async fn subxt_unpin_hash(api: &OnlineClient<PolkadotConfig>, sub_id: String, hash: H256) {
	let _ = api.rpc().chainhead_unstable_unpin(sub_id.clone(), hash).await;
}

fn on_error(e: SendError) {
	info!("Event consumer has terminated: {:?}, shutting down", e);
}

fn on_subscription_stop() {
	info!("Chain head subscription stopped");
}

fn on_ctrl_c() {
	info!("received interrupt signal shutting down subscription");
}
