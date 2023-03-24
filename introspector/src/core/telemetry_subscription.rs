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

use super::{EventConsumerInit, EventStream, TelemetryFeed, MAX_MSG_QUEUE_SIZE};
use async_trait::async_trait;
use color_eyre::Report;
use futures::future;
use futures_util::StreamExt;
use log::info;
use tokio::{
	net::TcpStream,
	sync::{
		broadcast::Sender as BroadcastSender,
		mpsc::{channel, error::SendError, Sender},
	},
};
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

#[async_trait]
impl EventStream for TelemetrySubscription {
	type Event = TelemetryEvent;

	/// Create a new consumer of events. Returns consumer initialization data.
	fn create_consumer(&mut self) -> EventConsumerInit<Self::Event> {
		let (update_tx, update_rx) = channel(MAX_MSG_QUEUE_SIZE);
		self.consumers.push(update_tx);

		EventConsumerInit::new(vec![update_rx])
	}

	async fn run(
		self,
		tasks: Vec<tokio::task::JoinHandle<()>>,
		shutdown_tx: BroadcastSender<()>,
	) -> color_eyre::Result<()> {
		let mut futures = self
			.consumers
			.into_iter()
			.map(|update_channel| tokio::spawn(Self::run_per_consumer(update_channel, shutdown_tx.clone())))
			.collect::<Vec<_>>();

		futures.extend(tasks);
		future::try_join_all(futures).await?;

		Ok(())
	}
}

impl TelemetrySubscription {
	pub fn new() -> Self {
		Self { consumers: Vec::new() }
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	async fn run_per_consumer(update_channel: Sender<TelemetryEvent>, shutdown_tx: BroadcastSender<()>) {
		let mut shutdown_rx = shutdown_tx.subscribe();
		let mut ws_stream = telemetry_stream().await;

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
						// TODO: change to info
						println!("[telemetry] {:?}", message);
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
}

async fn telemetry_stream() -> WebSocketStream<MaybeTlsStream<TcpStream>> {
	let url = Url::parse("wss://feed.telemetry.polkadot.io/feed/").unwrap();
	let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");

	ws_stream
}

fn on_consumer_error(e: SendError<TelemetryEvent>) {
	info!("Event consumer has terminated: {:?}, shutting down", e);
}

fn on_error(e: Report) {
	info!("Cannot parse telemetry feed: {:?}", e);
}

fn on_ctrl_c() {
	info!("received interrupt signal shutting down subscription");
}
