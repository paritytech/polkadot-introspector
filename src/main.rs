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

use clap::Parser;
use color_eyre::eyre::{eyre, WrapErr};
use log::{info, warn, LevelFilter};

#[subxt::subxt(runtime_metadata_path = "assets/rococo_metadata.scale")]
pub mod polkadot {}

mod block_time;
mod collector;
#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct CollectorOptions {
	/// Websockets url of a substrate node
	#[clap(name = "url", long, default_value = "ws://localhost:9944")]
	url: String,
	/// Maximum candidates to store
	#[clap(name = "max-candidates", long)]
	max_candidates: Option<usize>,
	/// Maximum age of candidates to preserve (default: 1 day)
	#[clap(name = "max-ttl", long, default_value = "86400")]
	max_ttl: usize,
}

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
	#[clap(name = "ws", long, default_value = "wss://westmint-rpc.polkadot.io:443")]
	nodes: String,
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

#[derive(Debug, Parser)]
#[clap(rename_all = "kebab-case")]
enum Command {
	BlockTimeMonitor(BlockTimeOptions),
	Collector(CollectorOptions),
}

#[derive(Debug, Parser)]
pub(crate) struct IntrospectorCli {
	#[clap(subcommand)]
	pub command: Command,
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, parse(from_occurrences))]
	pub verbose: i8,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;

	let opts = IntrospectorCli::parse();

	let log_level = match opts.verbose {
		0 => LevelFilter::Warn,
		1 => LevelFilter::Info,
		2 => LevelFilter::Debug,
		_ => LevelFilter::Trace,
	};

	env_logger::Builder::from_default_env()
		.filter(None, log_level)
		.format_timestamp(Some(env_logger::fmt::TimestampPrecision::Micros))
		.try_init()?;

	match opts.command {
		Command::Collector(opts) => {
			collector::run(opts).await?;
		},
		Command::BlockTimeMonitor(opts) => {
			block_time::BlockTimeMonitor::new(opts)?.run().await?;
		},
	}

	Ok(())
}
