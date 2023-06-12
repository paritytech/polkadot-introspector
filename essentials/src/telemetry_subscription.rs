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
use futures::{SinkExt, Stream, StreamExt};
use itertools::Itertools;
use log::{debug, info, warn};
use polkadot_introspector_priority_channel::{SendError, Sender};
use std::{
	cmp::{min, Reverse},
	collections::HashMap,
	io::{stdin, BufRead},
};
use tokio::{net::TcpStream, sync::broadcast::Sender as BroadcastSender};
use tokio_tungstenite::{
	connect_async,
	tungstenite::{Error as WsError, Message},
	MaybeTlsStream, WebSocketStream,
};

struct TelemetryStream(WebSocketStream<MaybeTlsStream<TcpStream>>);

impl TelemetryStream {
	async fn connect(url: &str) -> color_eyre::Result<Self, WsError> {
		info!("Connecting to the telemetry server on {}", url);
		match connect_async(url).await {
			Ok((ws_stream, _)) => Ok(Self(ws_stream)),
			Err(e) => Err(e),
		}
	}

	async fn subscribe_to(&mut self, chain: &H256) -> color_eyre::Result<(), WsError> {
		self.0.send(Message::Text(format!("subscribe:{:?}", chain))).await
	}
}

impl Stream for TelemetryStream {
	type Item = <WebSocketStream<MaybeTlsStream<TcpStream>> as Stream>::Item;

	fn poll_next(
		mut self: std::pin::Pin<&mut Self>,
		cx: &mut std::task::Context<'_>,
	) -> std::task::Poll<Option<Self::Item>> {
		std::pin::Pin::new(&mut self.0).poll_next(cx)
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
		// Usually, we avoid using String in function arguments to prevent unnecessary cloning.
		// However, we spawn this particular method as an asynchronous task, so we make an exception here.
		url: String,
		shutdown_tx: BroadcastSender<()>,
	) {
		let mut shutdown_rx = shutdown_tx.subscribe();
		let mut stream = match TelemetryStream::connect(&url).await {
			Ok(v) => v,
			Err(e) => return on_stream_error(e),
		};
		let mut subscribed: bool = false;
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
						debug!("[telemetry] {:?}", message);
						if !subscribed {
							if let TelemetryFeed::AddedChain(chain) = &message {
								chains.insert(chain.genesis_hash, chain.clone());
							}
						}
						if let Err(e) = update_channel.send(TelemetryEvent::NewMessage(message)).await {
							return on_consumer_error(e);
						}
					}

					if !subscribed {
						match choose_chain(&chains).await {
							Ok(hash) => {
								if let Err(e) = stream.subscribe_to(&hash).await {
									on_stream_error(e);
								} else {
									subscribed = true;
								}
							},
							Err(e) => {
								return on_choose_chain_error(e);
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
		url: &str,
		shutdown_tx: BroadcastSender<()>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let url = url;
		Ok(self
			.consumers
			.into_iter()
			.map(|update_channel| {
				tokio::spawn(Self::run_per_consumer(update_channel, url.to_string(), shutdown_tx.clone()))
			})
			.collect::<Vec<_>>())
	}
}

/// Number of chain choices displayed on a single screen
const CHAINS_CHUNK_SIZE: usize = 10;
/// Command that interupts user input
const EXIT_COMMAND: &str = "q";

#[derive(Debug, thiserror::Error)]
pub enum ChooseChainError {
	#[error("No chains found")]
	NoChains,
	#[error("Chain choice interupted by user")]
	NoChoice,
}

async fn choose_chain(chains: &HashMap<H256, AddedChain>) -> color_eyre::Result<H256, ChooseChainError> {
	let list: Vec<AddedChain> = chains
		.iter()
		.map(|(_, v)| v)
		.cloned()
		.sorted_by_key(|c| Reverse(c.node_count))
		.collect();

	if list.is_empty() {
		return Err(ChooseChainError::NoChains)
	}
	if list.len() == 1 {
		return Ok(list[0].genesis_hash)
	}

	println!("Connected to telemetry backend, {} chains found.\n", list.len());

	let chain_index: usize;
	let indexed_list: Vec<(usize, &AddedChain)> = list.iter().enumerate().map(|(i, c)| (i + 1, c)).collect();
	let mut cursor: usize = 0;
	loop {
		let mut buf = String::new();
		let chunk_range = cursor..min(cursor + CHAINS_CHUNK_SIZE, list.len());
		for (i, chain) in indexed_list[chunk_range].iter() {
			println!("{}. {}", i, chain);
		}
		println!(
			"\nInput the number of a chain you want to follow (1-{}).\nENTER to loop throw the list, {} to exit.\n",
			list.len(),
			EXIT_COMMAND.to_uppercase()
		);
		stdin().lock().read_line(&mut buf).expect("Failed to read line");

		buf = buf.trim().to_lowercase();

		if buf.is_empty() {
			cursor = if cursor + CHAINS_CHUNK_SIZE < list.len() { cursor + CHAINS_CHUNK_SIZE } else { 0 };
			continue
		}
		if buf == EXIT_COMMAND {
			return Err(ChooseChainError::NoChoice)
		}

		match buf.parse::<usize>() {
			Ok(num) => match num {
				1.. if num < list.len() => {
					chain_index = num - 1;
					break
				},
				_ => {
					println!("\nThe number should be between 1 and {}\n", list.len());
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

fn on_stream_error(e: WsError) {
	warn!("WebSocketError: {:?}", e);
}

fn on_error(e: Report) {
	warn!("Cannot parse telemetry feed: {:?}", e);
}

fn on_choose_chain_error(e: ChooseChainError) {
	warn!("Chain hasn't been chosen: {:?}", e);
}

fn on_ctrl_c() {
	info!("received interrupt signal shutting down subscription");
}
