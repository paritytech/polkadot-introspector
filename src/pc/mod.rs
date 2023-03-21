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
	EventConsumerInit, RequestExecutor, SubxtEvent,
};
use clap::Parser;
use colored::Colorize;
use crossterm::style::Stylize;
use itertools::Itertools;
use log::{error, info, warn};
use prometheus::{Metrics, ParachainCommanderPrometheusOptions};
use std::{collections::HashMap, default::Default, time::Duration};
use subxt::utils::H256;
use tokio::sync::{
	broadcast::{error::TryRecvError, Receiver as BroadcastReceiver, Sender as BroadcastSender},
	mpsc::Receiver,
};

use crate::{
	core::collector::{CollectorStorageApi, CollectorUpdateEvent},
	pc::tracker::SubxtTracker,
};

mod progress;
pub(crate) mod prometheus;
pub(crate) mod stats;
pub(crate) mod tracker;

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
	#[clap(long, conflicts_with = "all")]
	para_id: Vec<u32>,
	#[clap(long, conflicts_with = "para_id", default_value = "false")]
	all: bool,
	/// Run for a number of blocks then stop.
	#[clap(name = "blocks", long)]
	block_count: Option<u32>,
	/// Defines subscription mode
	/// The number of last blocks with missing slots to display
	#[clap(long = "last-skipped-slot-blocks", default_value = "10")]
	pub last_skipped_slot_blocks: usize,
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
		print_host_configuration(self.opts.node.as_str(), &mut collector.executor())
			.await
			.map_err(|e| {
				warn!("Cannot get host configuration: {}", e);
				e
			})
			.unwrap_or_default();

		println!(
			"{} will trace parachain(s) {} on {}\n{}",
			"Parachain Commander(TM)".to_string().purple(),
			self.opts.para_id.iter().join(","),
			&self.node,
			"-----------------------------------------------------------------------"
				.to_string()
				.bold()
		);

		if self.opts.all {
			let from_collector = collector.subscribe_broadcast_updates().await?;
			output_futures.push(tokio::spawn(ParachainCommander::watch_node_broadcast(
				self.clone(),
				from_collector,
				collector.api(),
			)));
		} else {
			for para_id in self.opts.para_id.iter() {
				let from_collector = collector.subscribe_parachain_updates(*para_id).await?;
				output_futures.push(tokio::spawn(ParachainCommander::watch_node_for_parachain(
					self.clone(),
					from_collector,
					*para_id,
					collector.api(),
				)));
			}
		}

		let consumer_channels: Vec<Receiver<SubxtEvent>> = consumer_config.into();
		let _collector_fut = collector
			.run_with_consumer_channel(consumer_channels.into_iter().next().unwrap())
			.await;

		Ok(output_futures)
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	async fn watch_node_for_parachain(
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
			self.opts.last_skipped_slot_blocks,
		);

		loop {
			match from_collector.try_recv() {
				Ok(update_event) => match update_event {
					CollectorUpdateEvent::NewHead(new_head) =>
						for relay_fork in &new_head.relay_parent_hashes {
							self.process_tracker_update(&mut tracker, *relay_fork, new_head.relay_parent_number)
								.await;
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

	async fn watch_node_broadcast(
		self,
		mut from_collector: BroadcastReceiver<CollectorUpdateEvent>,
		api_service: CollectorStorageApi,
	) {
		let mut trackers: HashMap<u32, SubxtTracker> = HashMap::new();
		let executor = api_service.subxt();

		loop {
			match from_collector.try_recv() {
				Ok(update_event) => match update_event {
					CollectorUpdateEvent::NewHead(new_head) =>
						for relay_fork in &new_head.relay_parent_hashes {
							let para_id = new_head.para_id;

							let tracker = trackers.entry(para_id).or_insert_with(|| {
								SubxtTracker::new(
									para_id,
									self.node.as_str(),
									executor.clone(),
									api_service.clone(),
									self.opts.last_skipped_slot_blocks,
								)
							});
							self.process_tracker_update(tracker, *relay_fork, new_head.relay_parent_number)
								.await;
						},
					CollectorUpdateEvent::NewSession(idx) =>
						for tracker in trackers.values_mut() {
							tracker.new_session(idx).await;
						},
					CollectorUpdateEvent::Termination => break,
				},
				Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(500)).await,
				Err(e) => {
					info!("Input channel has been closed: {:?}", e);
					break
				},
			}
		}

		for tracker in trackers.values() {
			let stats = tracker.summary();
			if matches!(self.opts.mode, Some(ParachainCommanderMode::Cli)) {
				print!("{}", stats);
			} else {
				info!("{}", stats);
			}
		}
	}

	async fn process_tracker_update(&self, tracker: &mut SubxtTracker, relay_hash: H256, relay_parent_number: u32) {
		match tracker.inject_block(relay_hash, relay_parent_number).await {
			Ok(_) => {
				if let Some(progress) = tracker.progress(&self.metrics) {
					if matches!(&self.opts.mode, Some(ParachainCommanderMode::Cli)) {
						println!("{}", progress);
					}
				}
				tracker.maybe_reset_state();
			},
			Err(e) => {
				error!("error occurred when processing block {}: {:?}", relay_hash, e)
			},
		}
	}
}

async fn print_host_configuration(url: &str, executor: &mut RequestExecutor) -> color_eyre::Result<()> {
	let conf = executor.get_host_configuration(url).await?;
	println!("Host configuration for {}:", url.to_owned().bold());
	println!(
		"\t👀 Max validators: {} / {} per core",
		format!("{}", conf.max_validators.unwrap_or(0)).bold(),
		format!("{}", conf.max_validators_per_core.unwrap_or(0)).bright_magenta(),
	);
	println!("\t👍 Needed approvals: {}", format!("{}", conf.needed_approvals).bold(),);
	println!("\t🥔 No show slots: {}", format!("{}", conf.no_show_slots).bold(),);
	println!("\t⏳ Delay tranches: {}", format!("{}", conf.n_delay_tranches).bold(),);
	Ok(())
}
