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
//! This is currently a work in progress, but there is a plan in place.
//! At this stage we can only use on-chain data to derive parachain metrics,
//! but later we can expand to use off-chain data as well like gossip.
//!
//! Features:
//! - backing and availability health metrics for all parachains
//! - parachain block times measured in relay chain blocks
//! - parachain XCM tput
//! - parachain code size
//!
//! Main usecase is debugging/development via CLI/TUI. CI integration also supported
//! via Prometheus metrics exporting.

use crate::core::{api::ApiService, EventConsumerInit, RecordsStorageConfig, SubxtEvent};
use clap::Parser;
use colored::Colorize;
use crossterm::{
	cursor,
	terminal::{Clear, ClearType},
	QueueableCommand,
};
use log::{debug, error, info, warn};
use tokio::sync::mpsc::{channel, Receiver, Sender};

use super::core::polkadot;

use std::{
	error::Error,
	io,
	time::{Duration, Instant},
};

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct ParachainCommanderOptions {
	/// Websocket url of a relay chain node.
	#[clap(name = "ws", long, value_delimiter = ',', default_value = "wss://rpc.polkadot.io:443")]
	pub node: String,
	/// Parachain id.
	#[clap(long)]
	para_id: u32,
}

pub(crate) struct ParachainCommander {
	opts: ParachainCommanderOptions,
	node: String,
	consumer_config: EventConsumerInit<SubxtEvent>,
	api_service: ApiService,
}

impl ParachainCommander {
	pub(crate) fn new(
		opts: ParachainCommanderOptions,
		consumer_config: EventConsumerInit<SubxtEvent>,
	) -> color_eyre::Result<Self> {
		// This starts the both the storage and subxt APIs.
		let api_service = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1000 });
		let node = opts.node.clone();

		Ok(ParachainCommander { opts, node, consumer_config, api_service })
	}

	// Spawn the UI and subxt tasks and return their futures.
	pub(crate) async fn run(self) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let consumer_channels: Vec<Receiver<SubxtEvent>> = self.consumer_config.into();

		let watcher_future = tokio::spawn(Self::watch_node(
			self.opts.clone(),
			self.node.clone(),
			// There is only one update channel (we only follow one RPC node).
			consumer_channels.into_iter().next().unwrap(),
			self.api_service.clone(),
		));

		Ok(vec![watcher_future])
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	async fn watch_node(
		opts: ParachainCommanderOptions,
		url: String,
		mut consumer_config: Receiver<SubxtEvent>,
		api_service: ApiService,
	) {
		// The subxt API request executor.
		let executor = api_service.subxt();
		let para_id = opts.para_id;

		// Break if user quits.
		loop {
			// Process all pending events.
			while let Ok(event) = consumer_config.try_recv() {
				match event {
					SubxtEvent::NewHead(hash) => {
						if let Some(header) = executor.get_block_head(url.clone(), Some(hash)).await {
							println!("New relay block build #{}, {}", header.number, header.hash());
						} else {
							error!("Failed to get header for {}", hash);
						}
					},
					_ => {}
				}
			}
		}
	}
}

