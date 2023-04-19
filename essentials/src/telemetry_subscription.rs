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

use super::telemetry_feed::TelemetryFeed;
use crate::{telemetry_feed::AddedChain, types::H256};
use color_eyre::Report;
use futures::{stream::Next, SinkExt};
use futures_util::StreamExt;
use itertools::Itertools;
use log::{debug, info, warn};
use priority_channel::{SendError, Sender};
use std::{
	cmp::{min, Reverse},
	collections::HashMap,
	io::{stdin, BufRead},
};
use tokio::{net::TcpStream, sync::broadcast::Sender as BroadcastSender};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

struct TelemetryStream {
	ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl TelemetryStream {
	async fn connect(url: &str) -> Self {
		println!("Connecting to the telemetry server on {}\n", url);
		let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");
		Self { ws_stream }
	}

	async fn subscribe_to(&mut self, chain: &H256) -> color_eyre::Result<()> {
		match self.ws_stream.send(Message::Text(format!("subscribe:{:?}", chain))).await {
			Ok(_) => {
				info!("Subscribed to chain with hash {}", chain);
				Ok(())
			},
			Err(e) => {
				info!("Cannot subscribe to chain with hash {}: {:?}", chain, e);
				Err(e.into())
			},
		}
	}

	fn next(&mut self) -> Next<'_, WebSocketStream<MaybeTlsStream<TcpStream>>> {
		self.ws_stream.next()
	}
}

#[derive(Debug)]
pub enum TelemetryEvent {
	/// New message
	NewMessage(TelemetryFeed),
}

pub struct TelemetrySubscription {
	/// One sender per consumer per URL.
	consumers: Vec<Sender<TelemetryEvent>>,
}

impl TelemetrySubscription {
	pub fn new(consumers: Vec<Sender<TelemetryEvent>>) -> Self {
		Self { consumers }
	}

	// Subscribes to a telemetry feed handling graceful shutdown.
	async fn run_per_consumer(
		mut update_channel: Sender<TelemetryEvent>,
		url: String,
		shutdown_tx: BroadcastSender<()>,
	) {
		let mut shutdown_rx = shutdown_tx.subscribe();
		let mut stream = TelemetryStream::connect(&url).await;
		let mut current_chain: H256 = H256::zero();
		let mut chains: HashMap<H256, AddedChain> = Default::default();

		loop {
			tokio::select! {
				Some(Ok(msg)) = stream.next() => {
					let bytes = match msg {
						Message::Text(text) => text.into_bytes(),
						Message::Binary(bytes) => bytes,
						_ => continue,
					};
					let feed = TelemetryFeed::from_bytes(&bytes);
					if let Err(e) = feed {
						on_error(e);
						continue;
					}

					for message in feed.unwrap() {
						if let TelemetryFeed::AddedChain(chain) = &message {
							chains.insert(chain.genesis_hash, chain.clone());
						}
						debug!("[telemetry] {:?}", message);
						if let Err(e) = update_channel.send(TelemetryEvent::NewMessage(message)).await {
							return on_consumer_error(e);
						}
					}

					if H256::is_zero(&current_chain) {
						match choose_chain(&chains).await {
							Ok(hash) => {
								current_chain = hash;
								let _ = stream.subscribe_to(&current_chain).await;
							},
							Err(_) => {
								return on_no_chains();
							}
						}
					}
				},
				_ = shutdown_rx.recv() => {
					return on_ctrl_c();
				}
			}
		}
	}

	pub async fn run(
		self,
		url: String,
		shutdown_tx: BroadcastSender<()>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		Ok(self
			.consumers
			.into_iter()
			.map(|update_channel| {
				tokio::spawn(Self::run_per_consumer(update_channel, url.clone(), shutdown_tx.clone()))
			})
			.collect::<Vec<_>>())
	}
}

const CHAINS_CHUNK_SIZE: usize = 10;

async fn choose_chain(chains: &HashMap<H256, AddedChain>) -> color_eyre::Result<H256, ()> {
	let list: Vec<AddedChain> = chains
		.iter()
		.map(|(_, v)| v.clone())
		.sorted_by_key(|c| Reverse(c.node_count))
		.collect();
	let list_size = list.len();

	match list_size {
		0 => {
			println!("No chains found...");
			return Err(())
		},
		1 => {
			println!("Found 1 chain.\n{}", list[0]);
			return Ok(list[0].genesis_hash)
		},
		size => {
			println!("Found {} chains.\n", size);
		},
	}

	let chain_index: usize;
	let indexed_list: Vec<(usize, &AddedChain)> = list.iter().enumerate().map(|(i, c)| (i + 1, c)).collect();
	let mut cursor: usize = 0;
	loop {
		let mut buf = String::new();
		for (i, chain) in indexed_list[cursor..min(cursor + CHAINS_CHUNK_SIZE, list_size)].iter() {
			println!("{}. {}", i, chain);
		}
		println!(
			"\nInput the number of a chain you want to follow (1-{}).\nENTER to loop throw the list, Q to exit.\n",
			list_size
		);
		stdin().lock().read_line(&mut buf).expect("Failed to read line");

		if buf == *"\n" {
			cursor = if cursor + CHAINS_CHUNK_SIZE < list_size { cursor + CHAINS_CHUNK_SIZE } else { 0 };
			continue
		}

		if buf.trim().to_lowercase() == *"q" {
			return Err(())
		}

		match buf.trim().parse::<usize>() {
			Ok(num) => match num {
				1.. if num < list_size => {
					chain_index = num - 1;
					break
				},
				_ => {
					println!("\nThe number should be between 1 and {}\n", list_size);
					continue
				},
			},
			Err(_) => continue,
		};
	}

	let selected = &list[chain_index];
	println!("\nSelected {}\n", selected);

	Ok(selected.genesis_hash)
}

fn on_consumer_error(e: SendError) {
	info!("Event consumer has terminated: {:?}, shutting down", e);
}

fn on_error(e: Report) {
	warn!("Cannot parse telemetry feed: {:?}", e);
}

fn on_no_chains() {
	warn!("No chains found, terminating...");
}

fn on_ctrl_c() {
	info!("received interrupt signal shutting down subscription");
}
