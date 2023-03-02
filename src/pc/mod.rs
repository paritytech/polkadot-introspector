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
//! - TODO: backing group information - validator addresses
//! - TODO: parachain block times measured in relay chain blocks
//! - TODO: parachain XCM throughput
//! - TODO: parachain code size
//!
//! The CLI interface is useful for debugging/diagnosing issues with the parachain block pipeline.
//! Soon: CI integration also supported via Prometheus metrics exporting.

use crate::core::{
	collector::{Collector, CollectorOptions},
	EventConsumerInit, SubxtEvent, SubxtSubscriptionMode,
};
use clap::Parser;
use color_eyre::owo_colors::OwoColorize;
use crossterm::style::Stylize;
use itertools::Itertools;
use log::info;
use std::{default::Default, time::Duration};
use tokio::sync::{
	broadcast::{error::TryRecvError, Receiver as BroadcastReceiver, Sender as BroadcastSender},
	mpsc::Receiver,
};

mod progress;
pub(crate) mod prometheus;
pub(crate) mod stats;
pub(crate) mod tracker;

use crate::core::collector::{CollectorStorageApi, CollectorUpdateEvent};
use prometheus::{Metrics, ParachainCommanderPrometheusOptions};
use tracker::ParachainBlockTracker;

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum ParachainCommanderMode {
	/// CLI chart mode.
	#[default]
	Cli,
	/// Prometheus endpoint mode.
	Prometheus(ParachainCommanderPrometheusOptions),
}

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct ParachainCommanderOptions {
	/// Web-Socket URLs of a relay chain node.
	#[clap(name = "ws", long, value_delimiter = ',', default_value = "wss://rpc.polkadot.io:443")]
	pub node: String,
	/// Parachain id.
	#[clap(long)]
	para_id: Vec<u32>,
	/// Run for a number of blocks then stop.
	#[clap(name = "blocks", long)]
	block_count: Option<u32>,
	/// Defines subscription mode
	#[clap(short = 's', long = "subscribe-mode", default_value_t, value_enum)]
	pub subscribe_mode: SubxtSubscriptionMode,
	#[clap(long = "last-skipped-slots", default_value = "10")]
	pub last_skipped_slots: usize,
	#[clap(flatten)]
	collector_opts: CollectorOptions,
	/// Mode of running - CLI/Prometheus. Default or no subcommand means `CLI` mode.
	#[clap(subcommand)]
	mode: Option<ParachainCommanderMode>,
}

#[derive(Clone)]
pub(crate) struct ParachainCommander {
	opts: ParachainCommanderOptions,
	node: String,
	metrics: Metrics,
}

impl ParachainCommander {
	pub(crate) fn new(mut opts: ParachainCommanderOptions) -> color_eyre::Result<Self> {
		// This starts the both the storage and subxt APIs.
		let node = opts.node.clone();
		opts.mode = opts.mode.or(Some(ParachainCommanderMode::Cli));

		Ok(ParachainCommander { opts, node, metrics: Default::default() })
	}

	/// Spawn the UI and subxt tasks and return their futures.
	pub(crate) async fn run(
		mut self,
		shutdown_tx: &BroadcastSender<()>,
		consumer_config: EventConsumerInit<SubxtEvent>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let mut output_futures = vec![];

		if let Some(ParachainCommanderMode::Prometheus(ref prometheus_opts)) = self.opts.mode {
			self.metrics = prometheus::run_prometheus_endpoint(prometheus_opts).await?;
		}

		let mut collector = Collector::new(self.opts.node.as_str(), self.opts.collector_opts.clone());
		collector.spawn(shutdown_tx).await?;

		println!(
			"{} will trace parachain(s) {} on {}\n{}",
			"Parachain Commander(TM)".to_string().purple(),
			self.opts.para_id.iter().join(","),
			&self.node,
			"-----------------------------------------------------------------------"
				.to_string()
				.bold()
		);

		for para_id in self.opts.para_id.iter() {
			let from_collector = collector.subscribe_parachain_updates(*para_id).await?;
			output_futures.push(tokio::spawn(ParachainCommander::watch_node(
				self.clone(),
				from_collector,
				*para_id,
				collector.api(),
			)));
		}

		let consumer_channels: Vec<Receiver<SubxtEvent>> = consumer_config.into();
		let _collector_fut = collector
			.run_with_consumer_channel(consumer_channels.into_iter().next().unwrap())
			.await;

		Ok(output_futures)
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	async fn watch_node(
		self,
		mut from_collector: BroadcastReceiver<CollectorUpdateEvent>,
		para_id: u32,
		api_service: CollectorStorageApi,
	) {
		// The subxt API request executor.
		let executor = api_service.subxt();
		let mut tracker = tracker::SubxtTracker::new(
			para_id,
			self.node.as_str(),
			executor,
			api_service,
			self.opts.last_skipped_slots,
		);

		loop {
			match from_collector.try_recv() {
				Ok(update_event) => match update_event {
					CollectorUpdateEvent::NewHead(new_head) =>
						for relay_fork in &new_head.relay_parent_hashes {
							tracker.inject_block(*relay_fork, new_head.relay_parent_number).await;
							if let Some(progress) = tracker.progress(&self.metrics) {
								if matches!(self.opts.mode, Some(ParachainCommanderMode::Cli)) {
									println!("{}", progress);
								}
							}
							tracker.maybe_reset_state();
						},
					CollectorUpdateEvent::NewSession(idx) => {
						tracker.new_session(idx).await;
					},
					CollectorUpdateEvent::Termination => break,
				},
				Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(500)).await,
				Err(_) => {
					info!("Input channel has been closed");
					break
				},
			}
		}

		let stats = tracker.summary();
		if matches!(self.opts.mode, Some(ParachainCommanderMode::Cli)) {
			print!("{}", stats);
		} else {
			info!("{}", stats);
		}
	}
}
