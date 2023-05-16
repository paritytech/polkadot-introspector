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
use log::{debug, error, info, warn};
use polkadot_introspector_essentials::{
	api::ApiService,
	chain_head_subscription::{ChainHeadEvent, ChainHeadSubscription},
	consumer::{EventConsumerInit, EventStream},
	init,
	storage::RecordsStorageConfig,
	types::H256,
	utils,
};
use polkadot_introspector_priority_channel::Receiver;
use prometheus_endpoint::{HistogramVec, Registry};
use std::{
	collections::VecDeque,
	io::{stdout, Write},
	net::ToSocketAddrs,
	sync::{
		atomic::{AtomicUsize, Ordering},
		Arc, Mutex,
	},
};
use subxt::config::Header;
use tokio::sync::broadcast;

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

struct BlockTimeMonitor {
	values: Vec<Arc<Mutex<VecDeque<u64>>>>,
	opts: BlockTimeOptions,
	block_time_metric: Option<HistogramVec>,
	endpoints: Vec<String>,
	consumer_config: EventConsumerInit<ChainHeadEvent>,
	api_service: ApiService<H256>,
	active_endpoints: Arc<AtomicUsize>,
}

impl BlockTimeMonitor {
	pub fn new(opts: BlockTimeOptions, consumer_config: EventConsumerInit<ChainHeadEvent>) -> color_eyre::Result<Self> {
		let endpoints = opts.nodes.clone();
		let mut values = Vec::new();
		for _ in 0..endpoints.len() {
			values.push(Default::default());
		}

		// This starts the both the storage and subxt APIs.
		let api_service = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1000 }, opts.retry.clone());
		let active_endpoints = Arc::new(AtomicUsize::new(endpoints.len()));

		match opts.clone().mode {
			BlockTimeMode::Prometheus(prometheus_opts) => {
				let prometheus_registry = Registry::new_custom(Some("introspector".into()), None)?;
				let block_time_metric = Some(register_metric(&prometheus_registry));

				let socket_addr_str = format!("{}:{}", prometheus_opts.address, prometheus_opts.port);
				socket_addr_str.to_socket_addrs()?.for_each(|addr| {
					tokio::spawn(prometheus_endpoint::init_prometheus(addr, prometheus_registry.clone()));
				});
				Ok(BlockTimeMonitor {
					values,
					opts,
					block_time_metric,
					endpoints,
					consumer_config,
					api_service,
					active_endpoints,
				})
			},
			BlockTimeMode::Cli(_) => Ok(BlockTimeMonitor {
				values,
				opts,
				block_time_metric: None,
				endpoints,
				consumer_config,
				api_service,
				active_endpoints,
			}),
		}
	}

	pub async fn run(self) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let consumer_channels: Vec<Receiver<ChainHeadEvent>> = self.consumer_config.into();

		let mut futures = self
			.endpoints
			.clone()
			.into_iter()
			.zip(self.values.clone().into_iter())
			.zip(consumer_channels.into_iter())
			.map(|((endpoint, values), update_channel)| {
				tokio::spawn(Self::watch_node(
					self.opts.clone(),
					endpoint,
					self.block_time_metric.clone(),
					values,
					update_channel,
					self.api_service.clone(),
					self.active_endpoints.clone(),
				))
			})
			.collect::<Vec<_>>();

		futures.push(tokio::spawn(Self::display_charts(
			self.values.clone(),
			self.endpoints.clone(),
			self.opts,
			self.active_endpoints,
		)));

		Ok(futures)
	}

	// TODO: get rid of arc mutex and use channels.
	async fn display_charts(
		values: Vec<Arc<Mutex<VecDeque<u64>>>>,
		endpoints: Vec<String>,
		opts: BlockTimeOptions,
		active_endpoints: Arc<AtomicUsize>,
	) {
		if let BlockTimeMode::Cli(opts) = opts.mode {
			loop {
				if active_endpoints.load(Ordering::Acquire) == 0 {
					// No more active endpoints remaining, give up
					info!("no more active endpoints are left, terminating UI loop");
					break
				}
				let _ = stdout().queue(Clear(ClearType::All)).unwrap();
				endpoints.iter().zip(values.iter()).enumerate().for_each(|(i, (uri, values))| {
					Self::display_chart(uri, (i * (opts.chart_height + 3)) as u32, values.clone(), opts.clone());
				});
				let _ = stdout().flush();
				tokio::time::sleep(std::time::Duration::from_secs(3)).await;
			}
		}
	}

	fn display_chart(uri: &str, row: u32, values: Arc<Mutex<VecDeque<u64>>>, opts: BlockTimeCliOptions) {
		use rasciigraph::{plot, Config};

		let _ = stdout().queue(cursor::MoveTo(0, row as u16));

		if values.lock().expect("Bad lock").is_empty() {
			return
		}

		// Get last `term_width` blocks.
		let blocks_to_show = opts.chart_width;

		// Remove old data points.
		let len = values.lock().expect("Bad lock").len();
		if len > blocks_to_show {
			values.lock().expect("Bad lock").drain(0..len - blocks_to_show);
		}

		let scaled_values: VecDeque<f64> =
			values.lock().expect("Bad lock").iter().map(|v| *v as f64 / 1000.0).collect();
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
		url: String,
		metric: Option<prometheus_endpoint::HistogramVec>,
		values: Arc<Mutex<VecDeque<u64>>>,
		// TODO: make this a struct.
		consumer_config: Receiver<ChainHeadEvent>,
		api_service: ApiService<H256>,
		active_endpoints: Arc<AtomicUsize>,
	) {
		// Make static string out of uri so we can use it as Prometheus label.
		let url = leak_static_str(url);
		match opts.clone().mode {
			BlockTimeMode::Prometheus(_) => {},
			BlockTimeMode::Cli(cli_opts) => {
				populate_view(values.clone(), url, cli_opts, api_service.clone()).await;
			},
		}
		let mut executor = api_service.subxt();

		let mut prev_ts = 0;
		let mut prev_block = 0u32;

		loop {
			debug!("[{}] New loop - waiting for events", url);
			if let Ok(event) = consumer_config.recv().await {
				debug!("New event: {:?}", event);
				let hash = match event {
					ChainHeadEvent::NewBestHead(hash) => Some(hash),
					ChainHeadEvent::NewFinalizedHead(hash) => Some(hash),
					ChainHeadEvent::Heartbeat => continue,
				};
				if let Some(hash) = hash {
					let ts = executor.get_block_timestamp(url, hash).await;
					let header = executor.get_block_head(url, Some(hash)).await;

					if let Ok(ts) = ts {
						if let Ok(Some(header)) = header {
							if prev_block != 0 && header.number.saturating_sub(prev_block) == 1 {
								// We know a prev block and this is it's child
								let block_time_ms = ts.saturating_sub(prev_ts);
								info!("[{}] Block time of #{}: {} ms", url, header.number, block_time_ms);

								match opts.mode {
									BlockTimeMode::Cli(_) => {
										values.lock().expect("Bad lock").push_back(block_time_ms);
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
					}
				} else {
					continue
				}
			} else {
				info!("[{}] Update channel disconnected", url);
				break
			}
		}
		active_endpoints.fetch_sub(1, Ordering::SeqCst);
	}
}

async fn populate_view(
	values: Arc<Mutex<VecDeque<u64>>>,
	url: &str,
	cli_opts: BlockTimeCliOptions,
	api_service: ApiService<H256>,
) {
	let mut prev_ts = 0u64;
	let blocks_to_fetch = cli_opts.chart_width;
	let mut executor = api_service.subxt();
	let mut parent_hash = None;

	for _ in 0..blocks_to_fetch {
		if let Ok(Some(header)) = executor.get_block_head(url, parent_hash).await {
			let ts = executor.get_block_timestamp(url, header.hash()).await.unwrap();

			if prev_ts != 0 {
				// We are walking backwards.
				let block_time_ms = prev_ts.saturating_sub(ts);
				values.lock().expect("Bad lock").insert(0, block_time_ms);
			}

			prev_ts = ts;
			parent_hash = Some(header.parent_hash);
		}
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
	let mut core = ChainHeadSubscription::new(opts.nodes.clone(), opts.retry.clone());
	let block_time_consumer_init = core.create_consumer();
	let (shutdown_tx, _) = broadcast::channel(1);

	match BlockTimeMonitor::new(opts, block_time_consumer_init)?.run().await {
		Ok(futures) =>
			core.run(futures, shutdown_tx.clone(), tokio::spawn(init::on_shutdown(shutdown_tx.clone())))
				.await?,
		Err(err) => error!("FATAL: cannot start block time monitor: {}", err),
	}

	Ok(())
}
