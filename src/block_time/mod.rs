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

use crate::core::{api::ApiService, EventConsumerInit, RecordsStorageConfig, SubxtEvent};
use clap::Parser;
use colored::Colorize;
use crossterm::{
	cursor,
	terminal::{Clear, ClearType},
	QueueableCommand,
};
use log::{debug, error, info, warn};
use prometheus_endpoint::{HistogramVec, Registry};
use std::{
	collections::VecDeque,
	io::{stdout, Write},
	sync::{Arc, Mutex},
};
use tokio::sync::mpsc::Receiver;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum BlockTimeMode {
	/// CLI chart mode.
	Cli(BlockTimeCliOptions),
	/// Prometheus endpoint mode.
	Prometheus(BlockTimePrometheusOptions),
}
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct BlockTimeOptions {
	/// Websockets url of a substrate nodes.
	#[clap(name = "ws", long, value_delimiter = ',', default_value = "wss://westmint-rpc.polkadot.io:443")]
	pub nodes: Vec<String>,
	/// Mode of running - cli/prometheus.
	#[clap(subcommand)]
	mode: BlockTimeMode,
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct BlockTimeCliOptions {
	/// Chart width.
	#[clap(long, default_value = "80")]
	chart_width: usize,
	/// Chart height.
	#[clap(long, default_value = "6")]
	chart_height: usize,
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct BlockTimePrometheusOptions {
	/// Prometheus endpoint port.
	#[clap(long, default_value = "65432")]
	port: u16,
}

pub(crate) struct BlockTimeMonitor {
	values: Vec<Arc<Mutex<VecDeque<u64>>>>,
	opts: BlockTimeOptions,
	block_time_metric: Option<HistogramVec>,
	endpoints: Vec<String>,
	consumer_config: EventConsumerInit<SubxtEvent>,
	api_service: ApiService,
}

impl BlockTimeMonitor {
	pub(crate) fn new(
		opts: BlockTimeOptions,
		consumer_config: EventConsumerInit<SubxtEvent>,
	) -> color_eyre::Result<Self> {
		let endpoints = opts.nodes.clone();
		let mut values = Vec::new();
		for _ in 0..endpoints.len() {
			values.push(Default::default());
		}

		// This starts the both the storage and subxt APIs.
		let api_service = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1000 });

		match opts.clone().mode {
			BlockTimeMode::Prometheus(prometheus_opts) => {
				let prometheus_registry = Registry::new_custom(Some("introspector".into()), None)?;
				let block_time_metric = Some(register_metric(&prometheus_registry));

				let socket_addr = std::net::SocketAddr::new(
					std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)),
					prometheus_opts.port,
				);
				tokio::spawn(prometheus_endpoint::init_prometheus(socket_addr, prometheus_registry));

				Ok(BlockTimeMonitor { values, opts, block_time_metric, endpoints, consumer_config, api_service })
			},
			BlockTimeMode::Cli(_) =>
				Ok(BlockTimeMonitor { values, opts, block_time_metric: None, endpoints, consumer_config, api_service }),
		}
	}

	pub(crate) async fn run(self) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let consumer_channels: Vec<Receiver<SubxtEvent>> = self.consumer_config.into();

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
				))
			})
			.collect::<Vec<_>>();

		futures.push(tokio::spawn(Self::display_charts(self.values.clone(), self.endpoints.clone(), self.opts)));

		Ok(futures)
	}

	// TODO: get rid of arc mutex and use channels.
	async fn display_charts(values: Vec<Arc<Mutex<VecDeque<u64>>>>, endpoints: Vec<String>, opts: BlockTimeOptions) {
		if let BlockTimeMode::Cli(opts) = opts.mode {
			loop {
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
		if len > blocks_to_show as usize {
			values.lock().expect("Bad lock").drain(0..len - (blocks_to_show as usize));
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
					format!("{:.2}", last).bright_purple().underline(),
					format!("{:.2}", avg).white().bold(),
					format!("{:.2}", min).green().bold(),
					format!("{:.2}", max).red().bold(),
					format!("Block production latency via '{}'", uri).yellow(),
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
		mut consumer_config: Receiver<SubxtEvent>,
		api_service: ApiService,
	) {
		// Make static string out of uri so we can use it as Prometheus label.
		let url = leak_static_str(url);
		match opts.clone().mode {
			BlockTimeMode::Prometheus(_) => {},
			BlockTimeMode::Cli(cli_opts) => {
				populate_view(values.clone(), url, cli_opts, api_service.clone()).await;
			},
		}
		let executor = api_service.subxt();

		let mut prev_ts = 0;
		let mut prev_block = 0u32;
		// tokio::time::sleep(std::time::Duration::from_secs(3000)).await;

		loop {
			debug!("[{}] New loop - waiting for events", url);
			if let Some(event) = consumer_config.recv().await {
				debug!("New event: {:?}", event);
				match event {
					SubxtEvent::NewHead(hash) => {
						let ts = executor.get_block_timestamp(url.into(), Some(hash)).await;
						let header = executor.get_block_head(url.into(), Some(hash)).await.unwrap();

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
					},
					_ => continue,
				}
			} else {
				error!("[{}] Update channel disconnected", url);
				break
			}
		}
	}
}

async fn populate_view(
	values: Arc<Mutex<VecDeque<u64>>>,
	url: &str,
	cli_opts: BlockTimeCliOptions,
	api_service: ApiService,
) {
	let mut prev_ts = 0u64;
	let blocks_to_fetch = cli_opts.chart_width;
	let executor = api_service.subxt();
	let mut parent_hash = None;

	for _ in 0..blocks_to_fetch {
		if let Some(header) = executor.get_block_head(url.into(), parent_hash).await {
			let ts = executor.get_block_timestamp(url.into(), Some(header.hash())).await;

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
