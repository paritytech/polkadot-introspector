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

use clap::Parser;
use colored::Colorize;
use crossterm::{
	cursor,
	terminal::{Clear, ClearType},
	QueueableCommand,
};
use log::{debug, info, warn};
use polkadot_introspector_essentials::{
	api::subxt_wrapper::{ApiClientMode, RequestExecutor},
	chain_head_subscription::ChainHeadSubscription,
	chain_subscription::ChainSubscriptionEvent,
	constants::MAX_MSG_QUEUE_SIZE,
	consumer::{EventConsumerInit, EventStream},
	init, utils,
};
use polkadot_introspector_priority_channel::{channel, Receiver, Sender};
use prometheus_endpoint::{HistogramVec, Registry};
use std::{
	collections::{HashMap, VecDeque},
	io::{stdout, Write},
	net::ToSocketAddrs,
};
use subxt::config::Header;
use tokio::select;

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Observe block times using an RPC node")]
struct BlockTimeOptions {
	/// Websockets URLs of a substrate nodes.
	#[clap(name = "ws", long, value_delimiter = ',')]
	pub nodes: Vec<String>,
	#[clap(subcommand)]
	mode: BlockTimeMode,
	#[clap(flatten)]
	pub verbose: init::VerbosityOptions,
	#[clap(flatten)]
	pub retry: utils::RetryOptions,
}

#[derive(Clone, Debug, Parser)]
enum BlockTimeMode {
	/// CLI chart mode.
	Cli(BlockTimeCliOptions),
	/// Prometheus endpoint mode.
	Prometheus(BlockTimePrometheusOptions),
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
struct BlockTimeCliOptions {
	/// Chart width.
	#[clap(long, default_value = "80")]
	chart_width: usize,
	/// Chart height.
	#[clap(long, default_value = "6")]
	chart_height: usize,
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
struct BlockTimePrometheusOptions {
	/// Address to bind Prometheus listener
	#[clap(short = 'a', long = "address", default_value = "0.0.0.0")]
	address: String,
	/// Port to bind Prometheus listener
	#[clap(short = 'p', long = "port", default_value = "65432")]
	port: u16,
}

#[derive(Debug)]
enum BlockTimeMessage {
	EndpointDisconected,
	NewBlockTime(String, u64),
}

struct BlockTimeMonitor {
	opts: BlockTimeOptions,
	block_time_metric: Option<HistogramVec>,
	endpoints: Vec<String>,
	executor: RequestExecutor,
	active_endpoints: usize,
}

impl BlockTimeMonitor {
	pub fn new(opts: BlockTimeOptions) -> color_eyre::Result<Self> {
		let executor = RequestExecutor::new(ApiClientMode::Online, opts.retry.clone());
		let endpoints = opts.nodes.clone();
		let active_endpoints = endpoints.len();

		match opts.clone().mode {
			BlockTimeMode::Prometheus(prometheus_opts) => {
				let prometheus_registry = Registry::new_custom(Some("introspector".into()), None)?;
				let block_time_metric = Some(register_metric(&prometheus_registry));

				let socket_addr_str = format!("{}:{}", prometheus_opts.address, prometheus_opts.port);
				socket_addr_str.to_socket_addrs()?.for_each(|addr| {
					tokio::spawn(prometheus_endpoint::init_prometheus(addr, prometheus_registry.clone()));
				});
				Ok(BlockTimeMonitor { opts, block_time_metric, endpoints, executor, active_endpoints })
			},
			BlockTimeMode::Cli(_) =>
				Ok(BlockTimeMonitor { opts, block_time_metric: None, endpoints, executor, active_endpoints }),
		}
	}

	pub async fn run(
		self,
		consumer_config: EventConsumerInit<ChainSubscriptionEvent>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let consumer_channels: Vec<Receiver<ChainSubscriptionEvent>> = consumer_config.into();
		let (message_tx, message_rx) = channel(MAX_MSG_QUEUE_SIZE);

		let mut futures = self
			.endpoints
			.clone()
			.into_iter()
			.zip(consumer_channels.into_iter())
			.map(|(endpoint, update_channel)| {
				tokio::spawn(Self::watch_node(
					self.opts.clone(),
					endpoint,
					self.block_time_metric.clone(),
					update_channel,
					self.executor.clone(),
					message_tx.clone(),
				))
			})
			.collect::<Vec<_>>();

		futures.push(tokio::spawn(Self::display_charts(
			self.endpoints.clone(),
			self.opts,
			self.active_endpoints,
			message_rx,
		)));

		Ok(futures)
	}

	async fn display_charts(
		endpoints: Vec<String>,
		opts: BlockTimeOptions,
		active_endpoints: usize,
		message_rx: Receiver<BlockTimeMessage>,
	) {
		if let BlockTimeMode::Cli(opts) = opts.mode {
			let mut values: HashMap<String, VecDeque<u64>> = HashMap::new();
			let mut update_interval = std::time::Duration::from_secs(0); // The first time to start at once

			loop {
				select! {
					message = message_rx.recv() => {
						match message {
							Ok(BlockTimeMessage::EndpointDisconected) =>  {
								let _ = active_endpoints.saturating_sub(1);
							},
							Ok(BlockTimeMessage::NewBlockTime(url, block_time)) => {
								values
									.entry(url)
									.and_modify(|v| {
										v.push_back(block_time);
										// Remove old data points.
										let len = v.len();
										if len > opts.chart_width {
											v.drain(0..len - opts.chart_width);
										}
									})
									.or_insert(VecDeque::from([block_time]));
							}
							_ => {}
						}
					}
					_ = tokio::time::sleep(update_interval) => {
						if active_endpoints == 0 {
							// No more active endpoints remaining, give up
							info!("no more active endpoints are left, terminating UI loop");
							break
						}
						let _ = stdout().queue(Clear(ClearType::All)).unwrap();

						endpoints.iter().enumerate().for_each(|(i, url)| {
							Self::display_chart(url, (i * (opts.chart_height + 3)) as u32, values.get(url), opts.clone());
						});
						let _ = stdout().flush();
						update_interval = std::time::Duration::from_secs(3);
					}
				}
			}
		}
	}

	fn display_chart(uri: &str, row: u32, values: Option<&VecDeque<u64>>, opts: BlockTimeCliOptions) {
		use rasciigraph::{plot, Config};

		let _ = stdout().queue(cursor::MoveTo(0, row as u16));
		if values.is_none() {
			return
		}

		// Get last `term_width` blocks.
		let blocks_to_show = opts.chart_width;
		let current_values = values.unwrap().clone();
		let len = current_values.len();

		let scaled_values: VecDeque<f64> = current_values.iter().map(|v| *v as f64 / 1000.0).collect();
		let avg: f64 = scaled_values.iter().sum::<f64>() / len as f64;
		let min: f64 = scaled_values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
		let max: f64 = scaled_values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
		let last = *scaled_values.back().unwrap_or(&0.0);
		let _ = stdout().write(
			plot(
				scaled_values.into(),
				Config::default().with_height(opts.chart_height as u32).with_caption(format!(
					"[DATA: {}] [LAST: {}] [AVG: {}] [MIN: {}] [MAX: {}] [ {} ]",
					blocks_to_show.to_string().bold(),
					format!("{last:.2}").bright_purple().underline(),
					format!("{avg:.2}").white().bold(),
					format!("{min:.2}").green().bold(),
					format!("{max:.2}").red().bold(),
					format!("Block production latency via '{uri}'").yellow(),
				)),
			)
			.as_bytes(),
		);
	}

	async fn watch_node(
		opts: BlockTimeOptions,
		url: String, // `String` rather than `&str` because we spawn this method as an asynchronous task
		metric: Option<prometheus_endpoint::HistogramVec>,
		// TODO: make this a struct.
		consumer_config: Receiver<ChainSubscriptionEvent>,
		mut executor: RequestExecutor,
		mut message_tx: Sender<BlockTimeMessage>,
	) {
		// Make static string out of uri so we can use it as Prometheus label.
		let url = leak_static_str(url);
		match opts.clone().mode {
			BlockTimeMode::Prometheus(_) => {},
			BlockTimeMode::Cli(cli_opts) => {
				populate_view(url, cli_opts, message_tx.clone(), executor.clone()).await;
			},
		}

		let mut prev_ts = 0;
		let mut prev_block = 0u32;

		loop {
			debug!("[{}] New loop - waiting for events", url);
			if let Ok(event) = consumer_config.recv().await {
				debug!("New event: {:?}", event);
				let (hash, header) = match event {
					ChainSubscriptionEvent::NewBestHead(v) => v,
					ChainSubscriptionEvent::NewFinalizedBlock(v) => v,
					ChainSubscriptionEvent::Heartbeat => continue,
				};
				let ts = executor.get_block_timestamp(url, hash).await;
				if let Ok(ts) = ts {
					if prev_block != 0 && header.number.saturating_sub(prev_block) == 1 {
						// We know a prev block and this is it's child
						let block_time_ms = ts.saturating_sub(prev_ts);
						info!("[{}] Block time of #{}: {} ms", url, header.number, block_time_ms);

						match opts.mode {
							BlockTimeMode::Cli(_) => {
								message_tx
									.send(BlockTimeMessage::NewBlockTime(url.to_string(), block_time_ms))
									.await
									.unwrap();
							},
							BlockTimeMode::Prometheus(_) =>
								if let Some(metric) = metric.clone() {
									metric.with_label_values(&[url]).observe(block_time_ms as f64)
								},
						}
					} else if prev_block != 0 && header.number.saturating_sub(prev_block) > 1 {
						// We know a prev block, but the diff is > 1. We lost blocks.
						// TODO(later): fetch the gap and publish the stats.
						// TODO(later later): Metrics tracking the missed blocks.
						warn!(
							"[{}] Missed {} blocks, likely because of WS connectivity issues",
							url,
							header.number.saturating_sub(prev_block).saturating_sub(1)
						);
					} else if prev_block == 0 {
						// Just starting up - init metric.
						if let Some(metric) = metric.clone() {
							metric.with_label_values(&[url]).observe(0f64)
						}
					}
					prev_ts = ts;
					prev_block = header.number;
				}
			} else {
				info!("[{}] Update channel disconnected", url);
				break
			}
		}

		message_tx.send(BlockTimeMessage::EndpointDisconected).await.unwrap();
	}
}

async fn populate_view(
	url: &str,
	cli_opts: BlockTimeCliOptions,
	mut message_tx: Sender<BlockTimeMessage>,
	mut executor: RequestExecutor,
) {
	let mut prev_ts = 0u64;
	let blocks_to_fetch = cli_opts.chart_width;
	let mut block_times: Vec<u64> = Vec::with_capacity(blocks_to_fetch);

	let mut parent_hash = None;

	for _ in 0..blocks_to_fetch {
		if let Ok(Some(header)) = executor.get_block_head(url, parent_hash).await {
			let ts = executor.get_block_timestamp(url, header.hash()).await.unwrap();

			if prev_ts != 0 {
				block_times.push(prev_ts.saturating_sub(ts));
			}

			prev_ts = ts;
			parent_hash = Some(header.parent_hash);
		}
	}
	// We are walking backwards.
	for block_time in block_times.into_iter().rev() {
		message_tx
			.send(BlockTimeMessage::NewBlockTime(url.to_string(), block_time))
			.await
			.unwrap();
	}
}

fn leak_static_str(string: String) -> &'static str {
	Box::leak(string.into_boxed_str())
}

fn register_metric(registry: &Registry) -> HistogramVec {
	prometheus_endpoint::register(
		HistogramVec::new(
			prometheus_endpoint::HistogramOpts::new("block_time", "Time it takes for blocks to be authored.")
				.buckets(vec![7000.0, 13000.0, 19000.0, 25000.0, 31000.0, 37000.0, 61000.0]),
			&["node"],
		)
		.unwrap(),
		registry,
	)
	.expect("Failed to register metric")
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	let opts = BlockTimeOptions::parse();
	init::init_cli(&opts.verbose)?;

	let monitor = BlockTimeMonitor::new(opts.clone())?;
	let shutdown_tx = init::init_shutdown();
	let mut futures = vec![];

	let mut sub = ChainHeadSubscription::new(opts.nodes.clone(), ApiClientMode::Online, opts.retry.clone());
	let consumer_init = sub.create_consumer();

	futures.extend(monitor.run(consumer_init).await?);
	futures.extend(sub.run(&shutdown_tx).await?);

	init::run(futures, &shutdown_tx).await?;

	Ok(())
}
