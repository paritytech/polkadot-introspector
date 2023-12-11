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

use clap::{error::ErrorKind, CommandFactory, Parser};
use colored::Colorize;
use crossterm::style::Stylize;
use futures::{future, stream::FuturesUnordered, StreamExt};
use itertools::Itertools;
use log::{error, info, warn};
use polkadot_introspector_essentials::{
	api::{api_client::ApiClientMode, executor::RpcExecutor},
	chain_head_subscription::ChainHeadSubscription,
	chain_subscription::ChainSubscriptionEvent,
	collector,
	collector::{Collector, CollectorOptions, CollectorStorageApi, CollectorUpdateEvent, TerminationReason},
	consumer::{EventConsumerInit, EventStream},
	historical_subscription::HistoricalSubscription,
	init,
	types::BlockNumber,
	utils::RetryOptions,
};
use polkadot_introspector_priority_channel::{channel_with_capacities, Receiver, Sender};
use prometheus::{Metrics, ParachainTracerPrometheusOptions};
use stats::ParachainStats;
use std::{collections::HashMap, default::Default, ops::DerefMut};
use tokio::sync::broadcast::Sender as BroadcastSender;
use tracker::SubxtTracker;
use tracker_rpc::ParachainTrackerRpc;
use tracker_storage::TrackerStorage;

mod message_queues_tracker;
mod parachain_block_info;
mod prometheus;
mod stats;
mod tracker;
mod tracker_rpc;
mod tracker_storage;
mod types;
mod utils;

#[cfg(test)]
mod test_utils;

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum ParachainTracerMode {
	/// CLI chart mode.
	#[default]
	Cli,
	/// Prometheus endpoint mode.
	Prometheus(ParachainTracerPrometheusOptions),
}

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Observe parachain state")]
pub(crate) struct ParachainTracerOptions {
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
	/// The number of last blocks with missing slots to display
	#[clap(long = "last-skipped-slot-blocks", default_value = "10")]
	pub last_skipped_slot_blocks: usize,
	/// Evict a stalled parachain after this amount of skipped blocks
	#[clap(long, default_value = "256")]
	max_parachain_stall: u32,
	/// Defines subscription mode
	#[clap(flatten)]
	collector_opts: CollectorOptions,
	/// Defines client to communicate with rpc node.
	#[clap(long = "client", default_value_t, value_enum)]
	pub api_client_mode: ApiClientMode,
	/// Run in historical mode to trace parachains between specific blocks instead of following live chain progress
	#[clap(name = "historical", long, requires = "from", requires = "to", conflicts_with = "subscribe_mode")]
	is_historical: bool,
	/// First block in historical mode, should be less then `--to` and the chain's tip
	#[clap(name = "from", long)]
	from_block_number: Option<BlockNumber>,
	/// Last block in historical mode, should be greater then `--from` and less then the chain's tip
	#[clap(name = "to", long)]
	to_block_number: Option<BlockNumber>,
	/// Mode of running - CLI/Prometheus. Default or no subcommand means `CLI` mode.
	#[clap(subcommand)]
	mode: Option<ParachainTracerMode>,
	#[clap(flatten)]
	pub verbose: init::VerbosityOptions,
	#[clap(flatten)]
	pub retry: RetryOptions,
}

#[derive(Clone)]
pub(crate) struct ParachainTracer {
	opts: ParachainTracerOptions,
	node: String,
	metrics: Metrics,
}

impl ParachainTracer {
	pub(crate) fn new(mut opts: ParachainTracerOptions) -> color_eyre::Result<Self> {
		// This starts the both the storage and subxt APIs.
		let node = opts.node.clone();
		opts.mode = opts.mode.or(Some(ParachainTracerMode::Cli));

		Ok(ParachainTracer { opts, node, metrics: Default::default() })
	}

	/// Spawn the UI and subxt tasks and return their futures.
	pub(crate) async fn run(
		mut self,
		shutdown_tx: &BroadcastSender<()>,
		consumer_config: EventConsumerInit<ChainSubscriptionEvent>,
		rpc_executor: &mut RpcExecutor,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let mut output_futures = vec![];

		if let Some(ParachainTracerMode::Prometheus(ref prometheus_opts)) = self.opts.mode {
			self.metrics = prometheus::run_prometheus_endpoint(prometheus_opts).await?;
		}

		let mut collector =
			Collector::new(self.opts.node.as_str(), self.opts.collector_opts.clone(), rpc_executor.clone());
		collector.spawn(shutdown_tx).await?;

		println!(
			"{}\n\twill trace {}\n\ton {}\n\tusing {} Client",
			"Parachain Tracer".to_string().purple(),
			if self.opts.all {
				"all parachain(s)".to_string()
			} else {
				format!("parachain(s) {}", self.opts.para_id.iter().join(","))
			},
			&self.node,
			self.opts.api_client_mode,
		);
		if let Err(e) = print_host_configuration(self.opts.node.as_str(), rpc_executor).await {
			warn!("Cannot get host configuration");
			return Err(e)
		}
		println!(
			"{}",
			"-----------------------------------------------------------------------"
				.to_string()
				.bold()
		);

		if self.opts.all {
			let from_collector = collector.subscribe_broadcast_updates().await?;
			output_futures.push(tokio::spawn(ParachainTracer::watch_node_broadcast(
				self.clone(),
				from_collector,
				collector.api(),
			)));
		} else {
			for para_id in self.opts.para_id.iter() {
				let from_collector = collector.subscribe_parachain_updates(*para_id).await?;
				output_futures.push(ParachainTracer::watch_node_for_parachain(
					self.clone(),
					from_collector,
					*para_id,
					collector.api(),
				));
			}
		}

		let consumer_channels: Vec<Receiver<ChainSubscriptionEvent>> = consumer_config.into();
		let collector_fut = collector
			.run_with_consumer_channel(consumer_channels.into_iter().next().unwrap())
			.await;

		output_futures.push(collector_fut);

		Ok(output_futures)
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	fn watch_node_for_parachain(
		self,
		from_collector: Receiver<CollectorUpdateEvent>,
		para_id: u32,
		api_service: CollectorStorageApi,
	) -> tokio::task::JoinHandle<()> {
		let mut rpc = ParachainTrackerRpc::new(para_id, self.node.as_str(), api_service.executor());
		let mut tracker = SubxtTracker::new(para_id);
		let storage = TrackerStorage::new(para_id, api_service.storage());

		let metrics = self.metrics.clone();
		let mut stats = ParachainStats::new(para_id, self.opts.last_skipped_slot_blocks);
		let is_cli = matches!(&self.opts.mode, Some(ParachainTracerMode::Cli));

		tokio::spawn(async move {
			loop {
				match from_collector.recv().await {
					Ok(update_event) => match update_event {
						CollectorUpdateEvent::NewHead(new_head) =>
							for relay_fork in &new_head.relay_parent_hashes {
								let parent_number = new_head.relay_parent_number;
								if let Err(e) =
									tracker.inject_block(*relay_fork, parent_number, &mut rpc, &storage).await
								{
									error!("error occurred when processing block {}: {:?}", relay_fork, e);
									std::process::exit(1);
								}
								if let Some(progress) = tracker.progress(&mut stats, &metrics, &storage).await {
									if is_cli {
										println!("{}", progress)
									}
								}
								tracker.maybe_reset_state();
							},
						CollectorUpdateEvent::NewSession(idx) => {
							tracker.inject_new_session(idx);
						},
						CollectorUpdateEvent::Termination(reason) => {
							info!("collector is terminating");
							match reason {
								TerminationReason::Normal => break,
								TerminationReason::Abnormal(code, info) => {
									error!("Shutting down, {}", info);
									std::process::exit(code)
								},
							}
						},
					},
					Err(_) => {
						info!("Input channel has been closed");
						break
					},
				}
			}

			if is_cli {
				print!("{}", stats);
			} else {
				info!("{}", stats);
			}
		})
	}

	async fn watch_node_broadcast(
		self,
		mut from_collector: Receiver<CollectorUpdateEvent>,
		api_service: CollectorStorageApi,
	) {
		let mut trackers = HashMap::new();
		// Used to track last block seen in parachain to evict stalled parachains
		// Another approach would be a BtreeMap indexed by a block number, but
		// for the practical reasons we are fine to do a hash map scan on each head.
		let mut last_blocks: HashMap<u32, u32> = HashMap::new();
		let mut best_known_block: u32 = 0;
		let max_stall = self.opts.max_parachain_stall;
		let mut futures = FuturesUnordered::new();

		loop {
			tokio::select! {
				message = from_collector.next() => {
					match message {
						Some(update_event) => match update_event {
							CollectorUpdateEvent::NewHead(new_head) => {
								let para_id = new_head.para_id;
								let last_known_block = new_head.relay_parent_number;

								let to_tracker = trackers.entry(para_id).or_insert_with(|| {
									let (tx, rx) = channel_with_capacities(collector::COLLECTOR_NORMAL_CHANNEL_CAPACITY, 1);
									futures.push(ParachainTracer::watch_node_for_parachain(self.clone(), rx, para_id, api_service.clone()));
									info!("Added tracker for parachain {}", para_id);

									tx
								});
								to_tracker.send(CollectorUpdateEvent::NewHead(new_head.clone())).await.unwrap();
								// Update last block number
								let _ = std::mem::replace(
									last_blocks.entry(para_id).or_insert(last_known_block).deref_mut(),
									last_known_block,
								);

								if last_known_block > best_known_block {
									best_known_block = last_known_block;
									evict_stalled(&mut trackers, &mut last_blocks, max_stall);
								}
							},
							CollectorUpdateEvent::NewSession(idx) =>
								for to_tracker in trackers.values_mut() {
									to_tracker.send(CollectorUpdateEvent::NewSession(idx)).await.unwrap();
								},
							CollectorUpdateEvent::Termination(reason) => {
								info!("Received termination event, {} trackers will be terminated, {} futures are pending",
									trackers.len(), futures.len());
								match reason {
									TerminationReason::Normal => break,
									TerminationReason::Abnormal(code, info) => {
										error!("Shutting down, {}", info);
										std::process::exit(code)
									},
								}
							},
						},
						None => {
							info!("Input channel has been closed");
							break;
						},
					};
				},
				Some(_) = futures.next() => {},
				else => break,
			}
		}

		// Drop all trackers channels to initiate their termination
		trackers.clear();
		future::try_join_all(futures).await.unwrap();
	}
}

fn evict_stalled(
	trackers: &mut HashMap<u32, Sender<CollectorUpdateEvent>>,
	last_blocks: &mut HashMap<u32, u32>,
	max_stall: u32,
) {
	let max_block = *last_blocks.values().max().unwrap_or(&0_u32);
	let to_evict: Vec<u32> = last_blocks
		.iter()
		.filter(|(_, last_block)| max_block - *last_block > max_stall)
		.map(|(para_id, _)| *para_id)
		.collect();
	for para_id in to_evict {
		let last_seen = last_blocks.remove(&para_id).expect("checked previously, qed");
		info!("evicting tracker for parachain {}, stalled for {} blocks", para_id, max_block - last_seen);
		trackers.remove(&para_id);
	}
}

async fn print_host_configuration(url: &str, executor: &mut RpcExecutor) -> color_eyre::Result<()> {
	let conf = executor.get_host_configuration(url).await?;
	println!("Host configuration for {}:", url.to_owned().bold());
	println!("{}", conf);
	Ok(())
}

fn historical_bounds(opts: &ParachainTracerOptions) -> color_eyre::Result<(u32, u32)> {
	let from_block_number = opts.from_block_number.expect("`--from` must exist in historical mode");
	let to_block_number = opts.to_block_number.expect("`--to` must exist in historical mode");
	if from_block_number >= to_block_number {
		let mut cmd = ParachainTracerOptions::command();
		cmd // Throws `clap` error instead of just panicking
			.error(ErrorKind::ArgumentConflict, "`--from` block number should be less then `--to`")
			.exit();
	}
	println!("Historical mode: from {} to {}", from_block_number, to_block_number);

	Ok((from_block_number, to_block_number))
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	let opts = ParachainTracerOptions::parse();
	init::init_cli(&opts.verbose)?;

	let tracer = ParachainTracer::new(opts.clone())?;
	let shutdown_tx = init::init_shutdown();

	let mut rpc_executor = RpcExecutor::new(opts.api_client_mode, opts.retry.clone());
	let mut futures = rpc_executor.start(opts.node.clone())?;

	let mut sub: Box<dyn EventStream<Event = ChainSubscriptionEvent>> = if opts.is_historical {
		let (from, to) = historical_bounds(&opts)?;
		Box::new(HistoricalSubscription::new(vec![opts.node.clone()], from, to, rpc_executor.clone()))
	} else {
		Box::new(ChainHeadSubscription::new(vec![opts.node.clone()], rpc_executor.clone()))
	};
	let consumer_init = sub.create_consumer();

	futures.extend(tracer.run(&shutdown_tx, consumer_init, &mut rpc_executor).await?);
	futures.extend(sub.run(&shutdown_tx).await?);

	init::run(futures, &shutdown_tx).await?;
	rpc_executor.close().await?;

	Ok(())
}
