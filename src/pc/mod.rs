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
	api::ApiService, EventConsumerInit, RecordsStorageConfig, SubxtDisputeResult, SubxtEvent, SubxtSubscriptionMode,
};
use clap::Parser;
use color_eyre::owo_colors::OwoColorize;
use crossterm::style::Stylize;
use log::info;
use std::{collections::HashMap, default::Default, time::Duration};
use subxt::ext::sp_core::H256;
use tokio::sync::mpsc::{error::TryRecvError, Receiver};

mod progress;
pub(crate) mod prometheus;
pub(crate) mod stats;
pub(crate) mod tracker;

use prometheus::{Metrics, ParachainCommanderPrometheusOptions};
use tracker::ParachainBlockTracker;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum ParachainCommanderMode {
	/// CLI chart mode.
	Cli,
	/// Prometheus endpoint mode.
	Prometheus(ParachainCommanderPrometheusOptions),
}

impl Default for ParachainCommanderMode {
	fn default() -> Self {
		ParachainCommanderMode::Cli
	}
}

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct ParachainCommanderOptions {
	/// Web-Socket URLs of a relay chain node.
	#[clap(name = "ws", long, value_delimiter = ',', default_value = "wss://rpc.polkadot.io:443")]
	pub node: String,
	/// Parachain id.
	#[clap(long)]
	para_id: u32,
	/// Run for a number of blocks then stop.
	#[clap(name = "blocks", long)]
	block_count: Option<u32>,
	/// Defines subscription mode
	#[clap(short = 's', long = "subscribe-mode", default_value_t, value_enum)]
	pub subscribe_mode: SubxtSubscriptionMode,
	/// Mode of running - CLI/Prometheus.
	#[clap(subcommand)]
	mode: Option<ParachainCommanderMode>,
}

pub(crate) struct ParachainCommander {
	opts: ParachainCommanderOptions,
	node: String,
	consumer_config: EventConsumerInit<SubxtEvent>,
	api_service: ApiService<H256>,
	metrics: Metrics,
}

impl ParachainCommander {
	pub(crate) fn new(
		opts: ParachainCommanderOptions,
		consumer_config: EventConsumerInit<SubxtEvent>,
	) -> color_eyre::Result<Self> {
		// This starts the both the storage and subxt APIs.
		let api_service = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1000 });
		let node = opts.node.clone();

		Ok(ParachainCommander { opts, node, consumer_config, api_service, metrics: Default::default() })
	}

	// Spawn the UI and subxt tasks and return their futures.
	pub(crate) async fn run(mut self) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let consumer_channels: Vec<Receiver<SubxtEvent>> = self.consumer_config.into();

		let mut output_futures = vec![];

		if let Some(ParachainCommanderMode::Prometheus(ref prometheus_opts)) = self.opts.mode {
			self.metrics = prometheus::run_prometheus_endpoint(prometheus_opts).await?;
		}

		let watcher_future = tokio::spawn(Self::watch_node(
			self.opts.clone(),
			self.node.clone(),
			// There is only one update channel (we only follow one RPC node).
			consumer_channels.into_iter().next().unwrap(),
			self.api_service,
			self.metrics,
		));

		output_futures.push(watcher_future);

		Ok(output_futures)
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	async fn watch_node(
		opts: ParachainCommanderOptions,
		url: String,
		mut consumer_config: Receiver<SubxtEvent>,
		api_service: ApiService<H256>,
		metrics: Metrics,
	) {
		// The subxt API request executor.
		let executor = api_service.subxt();
		let para_id = opts.para_id;
		// Track disputes that are unconcluded, value is the relay parent block number
		let mut recent_disputes_unconcluded: HashMap<H256, u32> = HashMap::new();
		// Keep concluded disputes per block
		let mut recent_disputes_concluded: HashMap<H256, (SubxtDisputeResult, Option<u32>)> = HashMap::new();

		let mut tracker = tracker::SubxtTracker::new(para_id, url.as_str(), executor);
		let mut start_block = None;
		let mut end_block = None;

		println!("{} will trace parachain {} on {}", "Parachain Commander(TM)".to_string().purple(), para_id, &url);
		println!(
			"{}",
			"-----------------------------------------------------------------------"
				.to_string()
				.bold()
		);

		// Break if user quits.
		loop {
			let recv_result = consumer_config.try_recv();
			match recv_result {
				Ok(event) => match event {
					SubxtEvent::NewHead(hash) => {
						tracker.inject_disputes_events(&recent_disputes_concluded);
						if let Some(progress) = tracker.progress(&metrics) {
							if matches!(opts.mode, Some(ParachainCommanderMode::Cli)) {
								println!("{}", progress);
							}
						}
						tracker.maybe_reset_state();
						recent_disputes_concluded.clear();
						let _parablock_info = tracker.inject_block(hash).await;

						if start_block.is_none() && opts.block_count.is_some() {
							start_block = tracker.get_current_relay_block_number();
							end_block = start_block.map(|block_number| {
								block_number + opts.block_count.expect("we just checked it above; qed") - 1
							});
						}

						if end_block.is_some() && tracker.get_current_relay_block_number() >= end_block {
							break
						}
					},
					SubxtEvent::DisputeInitiated(dispute) => {
						info!(
							"{}: relay parent: {:?}, candidate: {:?}",
							"Dispute initiated".to_string().dark_red(),
							dispute.relay_parent_block,
							dispute.candidate_hash,
						);
						recent_disputes_unconcluded.insert(
							dispute.candidate_hash,
							tracker.get_current_relay_block_number().unwrap_or_default(),
						);
					},
					SubxtEvent::DisputeConcluded(dispute, outcome) => {
						let str_outcome = match outcome {
							SubxtDisputeResult::Valid => format!("{}", "valid 👍".to_string().green()),
							SubxtDisputeResult::Invalid => format!("{}", "invalid 👎".to_string().dark_yellow()),
							SubxtDisputeResult::TimedOut => format!("{}", "timedout".to_string().dark_red()),
						};
						let noticed_dispute = recent_disputes_unconcluded.remove(&dispute.candidate_hash);
						let resolved_block_number = tracker.get_current_relay_block_number().unwrap_or_default();
						let resolved_time = noticed_dispute
							.map(|initiated_block| resolved_block_number.saturating_sub(initiated_block));

						recent_disputes_concluded.insert(dispute.candidate_hash, (outcome, resolved_time));
						info!(
							"{}: relay parent: {:?}, candidate: {:?}, result: {}",
							format!("Dispute concluded in {:?} blocks", resolved_time).bright_green(),
							dispute.relay_parent_block,
							dispute.candidate_hash,
							str_outcome,
						);
					},
					_ => {},
				},
				Err(TryRecvError::Disconnected) => break,
				Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(1000)).await,
			};
		}

		let stats = tracker.summary();
		if matches!(opts.mode, Some(ParachainCommanderMode::Cli)) {
			print!("{}", stats);
		} else {
			info!("{}", stats);
		}
	}
}
