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

use crate::types::H256;

use super::telemetry_feed::TelemetryFeed;
use color_eyre::Report;
use futures::SinkExt;
use futures_util::StreamExt;
use log::{debug, info, warn};
use priority_channel::{SendError, Sender};
use tokio::{net::TcpStream, sync::broadcast::Sender as BroadcastSender};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use url::Url;

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
		chain_hash: H256,
		shutdown_tx: BroadcastSender<()>,
	) {
		let mut shutdown_rx = shutdown_tx.subscribe();
		let mut ws_stream = telemetry_stream(&url, chain_hash).await;

		loop {
			tokio::select! {
				Some(Ok(msg)) = ws_stream.next() => {
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
						if let Err(e) = update_channel.send(TelemetryEvent::NewMessage(message)).await {
							return on_consumer_error(e);
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
		chain_hash: H256,
		shutdown_tx: BroadcastSender<()>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		Ok(self
			.consumers
			.into_iter()
			.map(|update_channel| {
				tokio::spawn(Self::run_per_consumer(update_channel, url.clone(), chain_hash, shutdown_tx.clone()))
			})
			.collect::<Vec<_>>())
	}
}

async fn telemetry_stream(url: &str, chain_hash: H256) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
	let url = Url::parse(url).unwrap();
	let (mut ws_stream, _) = connect_async(url).await.expect("Failed to connect");
	if let Err(e) = ws_stream.send(Message::Text(format!("subscribe:{:?}", chain_hash))).await {
		info!("Cannot subscribe to chain with hash {}: {:?}", chain_hash, e);
	}

	ws_stream
}

fn on_consumer_error(e: SendError) {
	info!("Event consumer has terminated: {:?}, shutting down", e);
}

fn on_error(e: Report) {
	warn!("Cannot parse telemetry feed: {:?}", e);
}

fn on_ctrl_c() {
	info!("received interrupt signal shutting down subscription");
}
