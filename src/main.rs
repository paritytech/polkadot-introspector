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
use color_eyre::eyre::eyre;
use log::{error, LevelFilter};
mod collector;

use block_time::BlockTimeOptions;
use collector::CollectorOptions;

mod block_time;
mod core;
mod jaeger;

use crate::core::EventStream;

#[derive(Debug, Parser)]
#[clap(rename_all = "kebab-case")]
enum Command {
	BlockTimeMonitor(BlockTimeOptions),
	Collector(CollectorOptions),
}

#[derive(Debug, Parser)]
#[clap(author, version)]
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
			let mut core = core::SubxtWrapper::new(opts.nodes.clone());
			let collector_consumer_init = core.create_consumer();

			match collector::run(opts, collector_consumer_init).await {
				Ok(futures) => core.run(futures).await?,
				Err(err) => error!("FATAL: cannot start collector: {}", err),
			}
		},
		Command::BlockTimeMonitor(opts) => {
			let mut core = core::SubxtWrapper::new(opts.nodes.clone());
			let block_time_consumer_init = core.create_consumer();

			match block_time::BlockTimeMonitor::new(opts, block_time_consumer_init)?.run().await {
				Ok(futures) => core.run(futures).await?,
				Err(err) => error!("FATAL: cannot start block time monitor: {}", err),
			}
		},
	}

	Ok(())
}
