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

use clap::{ArgAction, Parser};
use color_eyre::eyre::eyre;
use essentials::{consumer::EventStream, subxt_subscription::SubxtSubscription};
use futures::future;
use log::{error, LevelFilter};
use tokio::{
	signal,
	sync::broadcast::{self, Sender},
};

use block_time::BlockTimeOptions;
use jaeger::JaegerOptions;
use metadata_checker::{MetadataChecker, MetadataCheckerOptions};
use pc::ParachainCommanderOptions;
use telemetry::TelemetryOptions;

mod block_time;
mod core;
mod jaeger;
mod kvdb;
mod metadata_checker;
mod pc;
mod telemetry;

use crate::kvdb::KvdbOptions;

#[derive(Debug, Parser)]
#[clap(rename_all = "kebab-case")]
enum Command {
	/// Observe block times using an RPC node
	#[clap(aliases = &["blockmon"])]
	BlockTimeMonitor(BlockTimeOptions),
	/// Examine jaeger traces
	Jaeger(JaegerOptions),
	/// Examine key-value database for both relay chain and parachains
	Kvdb(KvdbOptions),
	/// Observe parachain state
	#[clap(aliases = &["pc"])]
	ParachainCommander(ParachainCommanderOptions),
	/// Validate statically generated metadata
	MetadataChecker(MetadataCheckerOptions),
	/// Simple telemetry feed
	Telemetry(TelemetryOptions),
}

#[derive(Debug, Parser)]
#[clap(author, version, about = "Introspection in the chain progress from a 🐦-view ")]
pub(crate) struct IntrospectorCli {
	#[clap(subcommand)]
	pub command: Command,
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, action = ArgAction::Count)]
	pub verbose: u8,
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
		Command::BlockTimeMonitor(opts) => {
			let mut core = SubxtSubscription::new(opts.nodes.clone());
			let block_time_consumer_init = core.create_consumer();
			let (shutdown_tx, _) = broadcast::channel(1);

			match block_time::BlockTimeMonitor::new(opts, block_time_consumer_init)?.run().await {
				Ok(mut futures) => {
					let shutdown_tx_cpy = shutdown_tx.clone();
					futures.push(tokio::spawn(async move {
						signal::ctrl_c().await.unwrap();
						let _ = shutdown_tx_cpy.send(());
					}));
					core.run(futures, shutdown_tx.clone()).await?
				},
				Err(err) => error!("FATAL: cannot start block time monitor: {}", err),
			}
		},
		Command::Jaeger(opts) => {
			let jaeger_cli = jaeger::JaegerTool::new(opts)?;
			match jaeger_cli.run().await {
				Ok(futures) => {
					let results = future::try_join_all(futures).await.map_err(|e| eyre!("Join error: {:?}", e))?;

					for res in results.iter() {
						if let Err(err) = res {
							error!("FATAL: {}", err);
						}
					}
				},
				Err(err) => error!("FATAL: cannot start jaeger command: {}", err),
			}
		},
		Command::Kvdb(opts) => {
			kvdb::introspect_kvdb(opts).await?;
		},
		Command::ParachainCommander(opts) => {
			let mut core = SubxtSubscription::new(vec![opts.node.clone()]);
			let consumer_init = core.create_consumer();
			let (shutdown_tx, _) = broadcast::channel(1);

			match pc::ParachainCommander::new(opts)?.run(&shutdown_tx, consumer_init).await {
				Ok(mut futures) => {
					let shutdown_tx_cpy = shutdown_tx.clone();
					futures.push(tokio::spawn(async move {
						signal::ctrl_c().await.unwrap();
						let _ = shutdown_tx_cpy.send(());
					}));
					core.run(futures, shutdown_tx.clone()).await?
				},
				Err(err) => error!("FATAL: cannot start parachain commander: {}", err),
			}
		},
		Command::MetadataChecker(opts) => {
			if let Err(err) = MetadataChecker::new(opts)?.run().await {
				error!("FATAL: cannot start metadata checker: {}", err)
			};
		},
		Command::Telemetry(opts) => {
			let shutdown_tx = init_shutdown();
			let futures = init_futures_with_shutdown(
				telemetry::Telemetry::new(opts)?.run(shutdown_tx.clone()).await?,
				shutdown_tx.clone(),
			);
			run(futures).await?
		},
	}

	Ok(())
}

fn init_shutdown() -> Sender<()> {
	let (shutdown_tx, _) = broadcast::channel(1);
	shutdown_tx
}

fn init_futures_with_shutdown(
	mut futures: Vec<tokio::task::JoinHandle<()>>,
	shutdown_tx: Sender<()>,
) -> Vec<tokio::task::JoinHandle<()>> {
	futures.push(tokio::spawn(on_shutdown(shutdown_tx)));
	futures
}

async fn on_shutdown(shutdown_tx: Sender<()>) {
	signal::ctrl_c().await.unwrap();
	let _ = shutdown_tx.send(());
}

async fn run(futures: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()> {
	future::try_join_all(futures).await?;
	Ok(())
}
