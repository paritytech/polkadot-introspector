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

use async_trait::async_trait;
use futures::future;
use log::{error, info};
use sp_core::H256;
use subxt::{ClientBuilder, DefaultConfig, DefaultExtra};

use tokio::sync::{
	mpsc::{channel, Receiver, Sender},
	oneshot,
	oneshot::error::TryRecvError,
};

use super::{
	Error, EventConsumerInit, EventStream, Request, RequestType, Response, Result, API_RETRY_TIMEOUT_MS,
	MAX_MSG_QUEUE_SIZE, RETRY_COUNT, RETRY_DELAY_MS,
};
use crate::polkadot;
use std::collections::hash_map::{Entry, HashMap};

pub struct SubxtWrapper {
	urls: Vec<String>,
	/// One sender per consumer per url.
	consumers: Vec<Vec<Sender<SubxtEvent>>>,
	api: Vec<Receiver<Request>>,
}

#[derive(Debug)]
pub enum SubxtEvent {
	NewHead(<DefaultConfig as subxt::Config>::Header),
	// TODO: RawEvent(subxt::RawEventDetails),
}

#[async_trait]
impl EventStream for SubxtWrapper {
	type Event = SubxtEvent;

	/// Create a new consumer of events. Returns consumer initialization data.
	fn create_consumer(&mut self) -> EventConsumerInit<Self::Event> {
		// Create API channel.
		let (to_api, api_rx) = channel(MAX_MSG_QUEUE_SIZE);
		let mut update_channels = Vec::new();

		// Create per ws update channels.
		for _ in 0..self.urls.len() {
			update_channels.push(channel(MAX_MSG_QUEUE_SIZE));
		}

		let (update_tx, update_channels) = update_channels.into_iter().unzip();

		// Keep per url update senders for this consumer.
		self.consumers.push(update_tx);
		self.api.push(api_rx);

		EventConsumerInit::new(update_channels, to_api)
	}

	async fn run(self, tasks: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()> {
		let futures = self
			.consumers
			.into_iter()
			.map(|update_channels| Self::run_per_consumer(update_channels, self.urls.clone()));

		let mut flat_futures = futures.flatten().collect::<Vec<_>>();
		flat_futures.push(tokio::spawn(Self::setup_api_handler(self.api)));
		flat_futures.extend(tasks);
		future::try_join_all(flat_futures).await?;

		Ok(())
	}
}

async fn subxt_get_head(
	api: &polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>,
	maybe_hash: Option<H256>,
) -> Result {
	Ok(Response::GetHeadResponse(api.client.rpc().header(maybe_hash).await.map_err(Error::SubxtError)?))
}

async fn subxt_get_block_ts(
	api: &polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>,
	maybe_hash: Option<H256>,
) -> Result {
	Ok(Response::GetBlockTimestampResponse(api.storage().timestamp().now(maybe_hash).await.map_err(Error::SubxtError)?))
}

impl SubxtWrapper {
	pub fn new(urls: Vec<String>) -> SubxtWrapper {
		SubxtWrapper { urls, consumers: Vec::new(), api: Vec::new() }
	}

	// Spawn API handler tasks.
	async fn setup_api_handler(apis: Vec<Receiver<Request>>) {
		apis.into_iter().for_each(|api| {
			tokio::spawn(Self::api_handler_task(api));
		});
	}

	// Attempts to connect to websocket and returns an RuntimeApi instance if successful.
	async fn new_client_fn(url: String) -> Option<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>> {
		for _ in 0..RETRY_COUNT {
			match ClientBuilder::new().set_url(url.clone()).build().await {
				Ok(api) =>
					return Some(
						api.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>(),
					),
				Err(err) => {
					error!("[{}] Client error: {:?}", url, err);
					tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
					continue
				},
			};
		}
		None
	}

	// Per consumer API task.
	async fn api_handler_task(mut api: Receiver<Request>) {
		let mut connection_pool = HashMap::new();
		loop {
			if let Some(request) = api.recv().await {
				let (timeout_sender, mut timeout_receiver) = oneshot::channel::<bool>();

				// Start API retry timeout task.
				let timeout_task = tokio::spawn(async move {
					tokio::time::sleep(std::time::Duration::from_millis(API_RETRY_TIMEOUT_MS)).await;
					timeout_sender.send(true).expect("Sending timeout signal never fails.");
				});

				// Loop while the timeout task doesn't fire. Other errors will cancel this loop
				loop {
					match timeout_receiver.try_recv() {
						Err(TryRecvError::Empty) => {},
						Err(TryRecvError::Closed) => {
							// Timeout task has exit unexpectedely. this shuld never happen.
							panic!("API timeout task closed channel: {:?}", request);
						},
						Ok(_) => {
							// Panic on timeout.
							panic!("Request timed out: {:?}", request);
						},
					}
					match connection_pool.entry(request.url.clone()) {
						Entry::Occupied(_) => (),
						Entry::Vacant(entry) => {
							let maybe_api = Self::new_client_fn(request.url.clone()).await;
							if let Some(api) = maybe_api {
								entry.insert(api);
							}
						},
					};

					let api = connection_pool.get(&request.url.clone());

					let result = if let Some(api) = api {
						match request.request_type {
							RequestType::GetBlockTimestamp(maybe_hash) => subxt_get_block_ts(api, maybe_hash).await,
							RequestType::GetHead(maybe_hash) => subxt_get_head(api, maybe_hash).await,
						}
					} else {
						// Remove the faulty websocket from connection pool.
						let _ = connection_pool.remove(&request.url);
						tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
						continue
					};

					let response = match result {
						Ok(response) => response,
						Err(Error::SubxtError(err)) => {
							error!("subxt call error: {:?}", err);
							// Always retry for subxt errors (most of them are transient).
							let _ = connection_pool.remove(&request.url);
							tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
							continue
						},
					};

					// We only break in the happy case.
					let _ = request.response_sender.send(response);
					timeout_task.abort();
					break
				}
			} else {
				// channel closed, exit loop.
				panic!("Channel closed. Cascade failure ?")
			}
		}
	}

	// Per consumer
	async fn run_per_node(update_channel: Sender<SubxtEvent>, url: String) {
		loop {
			match ClientBuilder::new().set_url(url.clone()).build().await {
				Ok(api) => {
					let api = api.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>();
					info!("[{}] Connected", url);
					match api.client.rpc().subscribe_blocks().await {
						Ok(mut sub) =>
							while let Some(ev_ctx) = sub.next().await {
								let header = ev_ctx.unwrap();
								info!("[{}] Block #{} imported ({:?})", url, header.number, header.hash());

								update_channel.send(SubxtEvent::NewHead(header.clone())).await.unwrap();
							},
						Err(err) => {
							error!("[{}] Disconnected ({:?}) ", url, err);
							// TODO (sometime): Add exponential backoff.
							tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
							info!("[{}] retrying connection ... ", url);
						},
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
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls.into_iter())
			.map(|(update_channel, url)| tokio::spawn(Self::run_per_node(update_channel, url)))
			.collect()
	}
}
