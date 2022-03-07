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
use futures::StreamExt;
use log::{info, warn};
use std::{
	net::SocketAddr,
	sync::{Arc, Mutex},
};
use subxt::{sp_core::H256, ClientBuilder, DefaultConfig, DefaultExtra};
use tokio::sync::{broadcast, oneshot};

mod candidate_record;
mod event_handler;
mod records_storage;
mod ws;

use crate::core::polkadot;
use candidate_record::*;
use color_eyre::eyre::{eyre, WrapErr};
use event_handler::*;
use records_storage::*;
use ws::*;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct CollectorOptions {
	/// Websockets url of a substrate node
	#[clap(name = "url", long, default_value = "ws://localhost:9944")]
	url: String,
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

pub(crate) async fn run(opts: CollectorOptions) -> color_eyre::Result<()> {
	let records_storage = Arc::new(Mutex::new(RecordsStorage::<H256, CandidateRecord<H256>>::new(opts.clone().into())));
	let (shutdown_tx, shutdown_rx) = oneshot::channel();
	let (updates_tx, mut updates_rx) = broadcast::channel(32);

	// TODO: might be useful to process multiple nodes in different tasks
	let api = ClientBuilder::new()
		.set_url(opts.url.clone())
		.build()
		.await
		.context("Error connecting to substrate node")?
		.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>();
	info!("Connected to a substrate node {}", &opts.url);
	let mut events_handler = EventsHandler::builder()
		.storage(records_storage.clone())
		.broadcast_tx(updates_tx)
		.build();
	let ws_listener = WebSocketListener::new(opts.clone().into(), records_storage.clone());

	let _ = ws_listener
		.spawn(shutdown_rx, updates_rx)
		.await
		.map_err(|e| eyre!("Cannot spawn a listener: {:?}", e))?;
	let mut event_sub = api.events().subscribe().await?;
	while let Some(events) = event_sub.next().await {
		let events = events?;
		let block_hash = events.block_hash();
		for event in events.iter_raw() {
			let event = event?;
			let _ = events_handler
				.handle_runtime_event(&event, &block_hash)
				.map_err(|e| warn!("cannot handle event: {:?}", e));
		}
	}

	let _ = shutdown_tx.send(());

	Ok(())
}
