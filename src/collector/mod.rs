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

use clap::Parser;
use futures::TryFutureExt;
use log::{debug, info, warn};
use std::ops::DerefMut;
use std::{net::SocketAddr, sync::Arc};
use subxt::sp_core::H256;
use tokio::{
	signal,
	sync::{
		broadcast,
		mpsc::{Receiver, Sender},
		Mutex,
	},
};

mod candidate_record;
mod event_handler;
mod records_storage;
mod ws;

use crate::core::{EventConsumerInit, Request, SubxtEvent};
use candidate_record::*;
use color_eyre::eyre::eyre;
use event_handler::*;
use records_storage::*;
use ws::*;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct CollectorOptions {
	#[clap(name = "ws", long, default_value = "wss://westmint-rpc.polkadot.io:443")]
	pub nodes: String,
	/// Maximum candidates to store
	#[clap(name = "max-candidates", long)]
	max_candidates: Option<usize>,
	/// Maximum age of candidates to preserve (default: 1 day)
	#[clap(name = "max-ttl", long, default_value = "86400")]
	max_ttl: usize,
	/// WS listen address to bind to
	#[clap(short = 'l', long = "listen", default_value = "0.0.0.0:3030")]
	listen_addr: SocketAddr,
}

impl From<CollectorOptions> for WebSocketListenerConfig {
	fn from(opts: CollectorOptions) -> WebSocketListenerConfig {
		WebSocketListenerConfig::builder().listen_addr(opts.listen_addr).build()
	}
}

impl From<CollectorOptions> for RecordsStorageConfig {
	fn from(opts: CollectorOptions) -> RecordsStorageConfig {
		RecordsStorageConfig { max_ttl: Some(opts.max_ttl), max_records: opts.max_candidates }
	}
}

pub(crate) async fn run(
	opts: CollectorOptions,
	consumer_config: EventConsumerInit<SubxtEvent>,
) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
	let records_storage = Arc::new(Mutex::new(RecordsStorage::<H256, CandidateRecord<H256>>::new(opts.clone().into())));
	let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
	let (updates_tx, updates_rx) = broadcast::channel(32);

	let endpoints: Vec<String> = opts.nodes.split(",").map(|s| s.to_owned()).collect();

	let (consumer_channels, _to_api): (Vec<Receiver<SubxtEvent>>, Sender<Request>) = consumer_config.into();
	let ws_listener = WebSocketListener::new(opts.clone().into(), records_storage.clone());
	let _ = ws_listener
		.spawn(shutdown_rx, updates_tx.clone())
		.await
		.map_err(|e| eyre!("Cannot spawn a listener: {:?}", e))?;

	let events_handler = Arc::new(Mutex::new(
		EventsHandler::builder()
			.storage(records_storage.clone())
			.broadcast_tx(updates_tx.clone())
			.build(),
	));

	let mut futures = endpoints
		.into_iter()
		.zip(consumer_channels.into_iter())
		.map(|(endpoint, mut update_channel)| {
			let events_handler = events_handler.clone();
			let mut shutdown_rx = shutdown_tx.subscribe();
			tokio::spawn(async move {
				loop {
					debug!("[{}] New loop - waiting for events", endpoint);
					tokio::select! {
						Some(event) = update_channel.recv() => {
							debug!("New event: {:?}", event);
							match event {
								SubxtEvent::RawEvent(hash, raw_ev) => {
									let _ = events_handler
										.lock()
										.await
										.deref_mut()
										.handle_runtime_event(&raw_ev, &hash)
										.map_err(|e| warn!("cannot handle event: {:?}", e));
								},
								_ => continue,
							};
						},
						_ = shutdown_rx.recv() => {
							info!("shutting down");
							break;
						}
					}
				}
			})
		})
		.collect::<Vec<_>>();

	futures.push(tokio::spawn(async move {
		signal::ctrl_c().await.unwrap();
		let _ = shutdown_tx.send(());
	}));
	Ok(futures)
}
