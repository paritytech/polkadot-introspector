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

use crate::prometheus::PrometheusMetrics;
use clap::{CommandFactory, Parser, error::ErrorKind};
use colored::Colorize;
use crossterm::style::Stylize;
use futures::{StreamExt, future, stream::FuturesUnordered};
use itertools::Itertools;
use log::{error, info, warn};
use polkadot_introspector_essentials::{
	api::{api_client::ApiClientMode, executor::RequestExecutor},
	chain_head_subscription::ChainHeadSubscription,
	chain_subscription::ChainSubscriptionEvent,
	collector::{self, Collector, CollectorOptions, CollectorStorageApi, CollectorUpdateEvent, TerminationReason},
	consumer::{EventConsumerInit, EventStream},
	historical_subscription::HistoricalSubscription,
	init::{self, Shutdown},
	types::BlockNumber,
	utils::{Retry, RetryOptions},
};
use polkadot_introspector_priority_channel::{Receiver, Sender, channel_with_capacities};
use prometheus::{Metrics, ParachainTracerPrometheusOptions};
use stats::ParachainStats;
use std::{collections::HashMap, default::Default, ops::DerefMut, time::Duration};
use tokio::sync::broadcast::Sender as BroadcastSender;
use tracker::SubxtTracker;
use tracker_storage::TrackerStorage;

mod message_queues_tracker;
mod parachain_block_info;
mod prometheus;
mod stats;
mod tracker;
mod tracker_storage;
mod types;
mod utils;

#[cfg(test)]
mod test_utils;

// HACK: We use a fake para id to receive only relay chain events
const RELAY_CHAIN_ID: u32 = 0;
const RELAY_CHAIN_ID_STR: &str = "0";

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
	pub(crate) fn new(mut opts: ParachainTracerOptions, metrics: Metrics) -> color_eyre::Result<Self> {
		// This starts the both the storage and subxt APIs.
		let node = opts.node.clone();
		opts.mode = opts.mode.or(Some(ParachainTracerMode::Cli));

		Ok(ParachainTracer { opts, node, metrics })
	}

	/// Spawn the UI and subxt tasks and return their futures.
	pub(crate) async fn run(
		self,
		shutdown_tx: &BroadcastSender<Shutdown>,
		consumer_config: EventConsumerInit<ChainSubscriptionEvent>,
		executor: &mut RequestExecutor,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let mut output_futures = vec![];

		let mut collector = Collector::new(self.opts.node.as_str(), self.opts.collector_opts.clone(), executor.clone());
		collector.spawn(shutdown_tx).await?;

		println!(
			"{}\n\twill trace {}\n\ton {}\n\tusing {} Client\n",
			"Parachain Tracer".to_string().purple(),
			if self.opts.all {
				"all parachain(s)".to_string()
			} else {
				format!("parachain(s) {}", self.opts.para_id.iter().join(","))
			},
			&self.node,
			self.opts.api_client_mode,
		);
		if let Err(e) = print_host_configuration(self.opts.node.as_str(), executor).await {
			warn!("Cannot get host configuration");
			return Err(e)
		}
		println!(
			"{}",
			"-----------------------------------------------------------------------"
				.to_string()
				.bold()
		);

		output_futures.push(ParachainTracer::watch_node_for_relay_chain(
			self.clone(),
			shutdown_tx.clone(),
			// HACK: We use a fake para id to receive only relay chain events
			collector.subscribe_parachain_updates(RELAY_CHAIN_ID).await?,
			collector.api(),
		));

		if self.opts.all {
			let from_collector = collector.subscribe_broadcast_updates().await?;
			output_futures.push(tokio::spawn(ParachainTracer::watch_node_broadcast(
				self.clone(),
				shutdown_tx.clone(),
				from_collector,
				collector.api(),
			)));
		} else {
			for para_id in self.opts.para_id.iter() {
				let from_collector = collector.subscribe_parachain_updates(*para_id).await?;
				output_futures.push(ParachainTracer::watch_node_for_parachain(
					self.clone(),
					shutdown_tx.clone(),
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

	fn watch_node_for_relay_chain(
		self,
		shutdown_tx: BroadcastSender<Shutdown>,
		from_collector: Receiver<CollectorUpdateEvent>,
		api_service: CollectorStorageApi,
	) -> tokio::task::JoinHandle<()> {
		let is_cli = matches!(&self.opts.mode, Some(ParachainTracerMode::Cli));
		let hasher = api_service.executor().hasher(&self.node).expect("Hasher must be available");
		let storage = TrackerStorage::new(0, api_service.storage(), hasher);
		let metrics = self.metrics.clone();

		let mut last_ts = None;

		tokio::spawn(async move {
			loop {
				match from_collector.recv().await {
					Ok(update_event) => match update_event {
						CollectorUpdateEvent::NewHead(new_head) => {
							// Happens on startup
							if new_head.relay_parent_number == 0 {
								continue
							}

							let curr_ts = storage
								.block_timestamp(
									*new_head
										.relay_parent_hashes
										.first()
										.expect("at least one relay parent hash must be present"),
								)
								.await;
							let block_time = last_ts
								.and_then(|last| curr_ts.map(|curr| curr.saturating_sub(last)))
								.map(Duration::from_millis);
							last_ts = curr_ts;
							let backed = new_head.candidates_backed.len();
							let included = new_head.candidates_included.len();
							let timed_out = new_head.candidates_timed_out.len();
							metrics.on_new_relay_block(
								backed,
								included,
								timed_out,
								block_time,
								new_head.relay_parent_number,
								new_head
									.authors_missing_their_slots
									.iter()
									.map(|account| account.to_string())
									.collect(),
							);
							if is_cli {
								println!(
									"Block {}: backed {}, included {}, timed-out {} {:}",
									new_head.relay_parent_number,
									backed,
									included,
									timed_out,
									if new_head.authors_missing_their_slots.is_empty() {
										"".to_string()
									} else {
										format!(
											", authors missing their slot: {}",
											new_head
												.authors_missing_their_slots
												.iter()
												.map(|account| account.to_string())
												.join(", ")
										)
									}
								);
							}
						},
						CollectorUpdateEvent::NewSession(session_id) => {
							metrics.on_new_session(session_id);
							if is_cli {
								println!("Session {}", session_id);
							}
						},
						CollectorUpdateEvent::Termination(reason) => {
							info!("collector is terminating");
							match reason {
								TerminationReason::Normal => break,
								TerminationReason::Abnormal(info) => {
									error!("Shutting down, {}", info);
									let _ = shutdown_tx.send(Shutdown::Restart);
									break;
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
		})
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	fn watch_node_for_parachain(
		self,
		shutdown_tx: BroadcastSender<Shutdown>,
		from_collector: Receiver<CollectorUpdateEvent>,
		para_id: u32,
		api_service: CollectorStorageApi,
	) -> tokio::task::JoinHandle<()> {
		let hasher = api_service.executor().hasher(&self.node).expect("Hasher must be available");
		let mut tracker = SubxtTracker::new(para_id);
		let storage = TrackerStorage::new(para_id, api_service.storage(), hasher);

		let metrics = self.metrics.clone();
		metrics.init_counters(para_id);
		let mut stats = ParachainStats::new(para_id, self.opts.last_skipped_slot_blocks);
		let is_cli = matches!(&self.opts.mode, Some(ParachainTracerMode::Cli));

		info!("Starting tracker for parachain {}", para_id);

		tokio::spawn(async move {
			loop {
				match from_collector.recv().await {
					Ok(update_event) => match update_event {
						CollectorUpdateEvent::NewHead(new_head) =>
							for relay_fork in &new_head.relay_parent_hashes {
								if let Err(e) = tracker.inject_block(*relay_fork, new_head.clone(), &storage).await {
									error!("error occurred when processing block {}: {:?}", relay_fork, e);
									let _ = shutdown_tx.send(Shutdown::Restart);
									break;
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
							info!("collector is terminating for parachain {}", para_id);
							match reason {
								TerminationReason::Normal => break,
								TerminationReason::Abnormal(info) => {
									error!("Shutting down, {}", info);
									let _ = shutdown_tx.send(Shutdown::Restart);
									break;
								},
							}
						},
					},
					Err(_) => {
						info!("Input channel has been closed for parachain {}", para_id);
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
		shutdown_tx: BroadcastSender<Shutdown>,
		from_collector: Receiver<CollectorUpdateEvent>,
		api_service: CollectorStorageApi,
	) {
		let mut trackers = HashMap::new();
		// Used to track last block seen in parachain to evict stalled parachains
		// Another approach would be a BtreeMap indexed by a block number, but
		// for the practical reasons we are fine to do a hash map scan on each head.
		let mut last_blocks: HashMap<u32, u32> = HashMap::new();
		let mut best_known_block: u32 = 0;
		let mut futures = FuturesUnordered::new();
		let mut from_collector = Box::pin(from_collector);

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
									futures.push(ParachainTracer::watch_node_for_parachain(self.clone(), shutdown_tx.clone(), rx, para_id, api_service.clone()));
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
									evict_stalled(&mut trackers, &mut last_blocks);
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
									TerminationReason::Abnormal(info) => {
										error!("Shutting down, {}", info);
										let _ = shutdown_tx.send(Shutdown::Restart);
										break;
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
		let _ = future::try_join_all(futures).await;
	}
}

fn evict_stalled(trackers: &mut HashMap<u32, Sender<CollectorUpdateEvent>>, last_blocks: &mut HashMap<u32, u32>) {
	// Collectors keep sending events to stalled paras for a particular amount of blocks.
	// So we can remove it almost immediately after we stopped receiving events.
	let max_block = *last_blocks.values().max().unwrap_or(&0_u32);
	let threshold = max_block.saturating_sub(10);
	let to_evict: Vec<u32> = last_blocks
		.iter()
		.filter_map(|(para_id, last_block)| if threshold > *last_block { Some(*para_id) } else { None })
		.collect();
	for para_id in to_evict {
		let last_seen = last_blocks.remove(&para_id).expect("checked previously, qed");
		info!("evicting tracker for parachain {}, stalled for {} blocks", para_id, max_block - last_seen);
		trackers.remove(&para_id);
	}
}

async fn print_host_configuration(url: &str, executor: &mut RequestExecutor) -> color_eyre::Result<()> {
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

	let mut retry = Retry::new(&opts.retry);

	let metrics = if let Some(ParachainTracerMode::Prometheus(ref prometheus_opts)) = opts.mode {
		prometheus::run_prometheus_endpoint(prometheus_opts).await?
	} else {
		Default::default()
	};

	loop {
		let tracer = ParachainTracer::new(opts.clone(), metrics.clone())?;
		let shutdown_tx = init::init_shutdown();
		let mut shutdown_rx = shutdown_tx.subscribe();
		let mut executor =
			match RequestExecutor::build(opts.node.clone(), opts.api_client_mode, &opts.retry, &shutdown_tx).await {
				Ok(executor) => executor,
				Err(e) => {
					error!("Failed to build RequestExecutor: {:?}", e);
					if retry.sleep().await.is_err() {
						break;
					}
					continue;
				},
			};

		let mut sub: Box<dyn EventStream<Event = ChainSubscriptionEvent>> = if opts.is_historical {
			let (from, to) = historical_bounds(&opts)?;
			Box::new(HistoricalSubscription::new(vec![opts.node.clone()], from, to, executor.clone()))
		} else {
			Box::new(ChainHeadSubscription::new(vec![opts.node.clone()], executor.clone()))
		};
		let consumer_init = sub.create_consumer();

		let mut futures = vec![];
		futures.extend(tracer.run(&shutdown_tx, consumer_init, &mut executor).await?);
		futures.extend(sub.run(&shutdown_tx).await?);
		init::run(futures, &shutdown_tx).await?;
		executor.close().await;

		let should_break = match shutdown_rx.recv().await {
			Ok(Shutdown::Restart) => retry.sleep().await.is_err(),
			_ => true,
		};

		if should_break {
			break;
		}
	}

	Ok(())
}
