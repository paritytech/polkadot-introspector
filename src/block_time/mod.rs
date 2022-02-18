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

use crate::{BlockTimeCliOptions, BlockTimeMode, BlockTimeOptions, BlockTimePrometheusOptions};
use color_eyre::eyre::WrapErr;
use colored::Colorize;
use crossterm::{
	cursor,
	terminal::{Clear, ClearType},
	QueueableCommand,
};
use futures::future;
use log::{debug, error, info, warn};
use prometheus_endpoint::{HistogramVec, Registry};
use std::{
	collections::VecDeque,
	io::{stdout, Write},
	sync::{Arc, Mutex},
};
use subxt::{ClientBuilder, DefaultConfig, DefaultExtra};

use crate::polkadot;

pub(crate) struct BlockTimeMonitor {
	values: Vec<Arc<Mutex<VecDeque<u64>>>>,
	opts: BlockTimeOptions,
	block_time_metric: Option<HistogramVec>,
	endpoints: Vec<String>,
}

impl BlockTimeMonitor {
	pub(crate) fn new(opts: BlockTimeOptions) -> color_eyre::Result<Self> {
		let endpoints: Vec<String> = opts.nodes.split(",").map(|s| s.to_owned()).collect();
		let mut values = Vec::new();
		for _ in 0..endpoints.len() {
			values.push(Default::default());
		}

		match opts.clone().mode {
			BlockTimeMode::Prometheus(prometheus_opts) => {
				let prometheus_registry = Registry::new_custom(Some("introspector".into()), None)?;
				let block_time_metric = Some(register_metric(&prometheus_registry));

				let socket_addr = std::net::SocketAddr::new(
					std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)),
					prometheus_opts.port,
				);
				tokio::spawn(prometheus_endpoint::init_prometheus(socket_addr, prometheus_registry));

				Ok(BlockTimeMonitor { values, opts, block_time_metric, endpoints })
			},
			BlockTimeMode::Cli(_) => Ok(BlockTimeMonitor { values, opts, block_time_metric: None, endpoints }),
		}
	}

	pub(crate) async fn run(self) -> color_eyre::Result<()> {
		let mut futures = self
			.endpoints
			.clone()
			.into_iter()
			.zip(self.values.clone().into_iter())
			.map(|(endpoint, values)| {
				tokio::spawn(Self::watch_node(self.opts.clone(), endpoint, self.block_time_metric.clone(), values))
			})
			.collect::<Vec<_>>();

		futures.push(tokio::spawn(self.display_charts()));
		future::try_join_all(futures).await?;

		Ok(())
	}

	async fn display_charts(self) {
		match self.opts.clone().mode {
			BlockTimeMode::Cli(opts) => loop {
				let _ = stdout().queue(Clear(ClearType::All)).unwrap();
				self.endpoints
					.iter()
					.zip(self.values.iter())
					.enumerate()
					.for_each(|(i, (uri, values))| {
						Self::display_chart(uri, (i * (opts.chart_height + 3)) as u32, values.clone(), opts.clone());
					});
				let _ = stdout().flush();
				tokio::time::sleep(std::time::Duration::from_secs(3)).await;
			},
			_ => {},
		};
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
			format!(
				"{}",
				plot(
					scaled_values.into(),
					Config::default().with_height(opts.chart_height as u32).with_caption(format!(
						"[DATA: {}] [LAST: {}] [AVG: {}] [MIN: {}] [MAX: {}] [ {} ]",
						format!("{}", blocks_to_show).bold(),
						format!("{:.2}", last).bright_purple().underline(),
						format!("{:.2}", avg).white().bold(),
						format!("{:.2}", min).green().bold(),
						format!("{:.2}", max).red().bold(),
						format!("Block production latency via '{}'", uri).yellow(),
					))
				)
			)
			.as_bytes(),
		);
	}

	async fn watch_node(
		opts: BlockTimeOptions,
		uri: String,
		metric: Option<prometheus_endpoint::HistogramVec>,
		values: Arc<Mutex<VecDeque<u64>>>,
	) {
		// Make static string out of uri so we can use it as Prometheus label.
		let uri = as_static_str(uri);
		match opts.clone().mode {
			BlockTimeMode::Prometheus(_) => {},
			BlockTimeMode::Cli(cli_opts) => {
				populate_view(values.clone(), uri, cli_opts).await;
			},
		}

		let mut prev_ts = 0;
		let mut prev_block = 0u32;

		// Loop forever and retry WS connections.
		loop {
			match ClientBuilder::new()
				.set_url(uri)
				.build()
				.await
				.context("Error connecting to substrate node")
			{
				Ok(api) => {
					let api = api.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>();
					info!("[{}] Connected", uri);
					let mut sub = api.client.rpc().subscribe_blocks().await.unwrap();

					while let Some(ev_ctx) = sub.next().await {
						let header = ev_ctx.unwrap();
						debug!("[{}] Block #{} imported ({:?})", uri, header.number, header.hash());
						let ts = api.storage().timestamp().now(Some(header.hash())).await.unwrap();
						debug!("[{}] Block #{} timestamp: {}", uri, header.number, ts);

						if prev_block != 0 && header.number.saturating_sub(prev_block) == 1 {
							// We know a prev block and this is it's child
							let block_time_ms = ts.saturating_sub(prev_ts);
							info!("[{}] Block time of #{}: {} ms", uri, header.number, block_time_ms);

							match opts.mode {
								BlockTimeMode::Cli(_) => {
									values.lock().expect("Bad lock").push_back(block_time_ms);
								},
								BlockTimeMode::Prometheus(_) => {
									metric
										.clone()
										.map(|metric| metric.with_label_values(&[uri]).observe(block_time_ms as f64));
								},
							}
						} else if prev_block != 0 && header.number.saturating_sub(prev_block) > 1 {
							// We know a prev block, but the diff is > 1. We lost blocks.
							// TODO(later): fetch the gap and publish the stats.
							// TODO(later later): Metrics tracking the missed blocks.
							warn!(
								"[{}] Missed {} blocks, likely because of WS connectivity issues",
								uri,
								header.number.saturating_sub(prev_block).saturating_sub(1)
							);
						} else if prev_block == 0 {
							// Just starting up - init metric.
							metric.clone().map(|metric| metric.with_label_values(&[uri]).observe(0f64));
						}
						prev_ts = ts;
						prev_block = header.number;
					}
				},
				Err(err) => {
					error!("[{}] Disconnected ({:?}) ", uri, err);
					tokio::time::sleep(std::time::Duration::from_millis(500)).await;
					info!("[{}] retrying connection ... ", uri);
				},
			}
		}
	}
}

async fn populate_view(values: Arc<Mutex<VecDeque<u64>>>, uri: &str, cli_opts: BlockTimeCliOptions) {
	let mut header;
	let mut prev_ts = 0u64;
	let mut prev_block = 0u32;
	// Get last `term_width` blocks.
	let mut blocks_to_fetch = cli_opts.chart_width;
	// Loop for ws connection retry.
	loop {
		match ClientBuilder::new()
			.set_url(uri)
			.build()
			.await
			.context("Error connecting to substrate node")
		{
			Ok(api) => {
				let api = api.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>();
				header = api.client.rpc().header(None).await.unwrap().unwrap();
				while blocks_to_fetch > 0 {
					let ts = match api.storage().timestamp().now(Some(header.hash())).await {
						Ok(ts) => ts,
						Err(_) => break,
					};

					if prev_block != 0 {
						// We are walking backwards.
						let block_time_ms = prev_ts.saturating_sub(ts);
						values.lock().expect("Bad lock").push_back(block_time_ms);
					}

					prev_ts = ts;
					prev_block = header.number;

					header = match api.client.rpc().header(Some(header.parent_hash)).await {
						Ok(maybe_header) => maybe_header.unwrap(),
						Err(_) => break,
					};
					blocks_to_fetch = blocks_to_fetch - 1;
				}
			},
			Err(_) => {
				tokio::time::sleep(std::time::Duration::from_millis(100)).await;
			},
		}
		if blocks_to_fetch == 0 {
			break
		}
	}
}

fn as_static_str(string: String) -> &'static str {
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
		&registry,
	)
	.expect("Failed to register metric")
}
