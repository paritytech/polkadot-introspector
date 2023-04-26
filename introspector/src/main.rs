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
use log::{error, LevelFilter};
use pc::ParachainCommanderOptions;
use polkadot_introspector_essentials::{
	consumer::EventStream, subxt_subscription::SubxtSubscription, utils::RetryOptions,
};
use tokio::{signal, sync::broadcast};

mod pc;

#[derive(Debug, Parser)]
#[clap(rename_all = "kebab-case")]
enum Command {
	/// Observe parachain state
	#[clap(aliases = &["pc"])]
	ParachainCommander(ParachainCommanderOptions),
}

#[derive(Debug, Parser)]
#[clap(author, version, about = "Introspection in the chain progress from a 🐦-view ")]
pub(crate) struct IntrospectorCli {
	#[clap(subcommand)]
	pub command: Command,
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, action = ArgAction::Count)]
	pub verbose: u8,
	/// Defines parameters for connection retry attempts
	#[clap(flatten)]
	pub retry_opts: RetryOptions,
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
	}

	Ok(())
}
