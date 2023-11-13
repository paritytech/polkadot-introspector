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

use clap::{Args, Parser, Subcommand};
use polkadot_introspector_essentials::{
	api::subxt_wrapper::{ApiClientMode, RequestExecutor, SubxtWrapperError},
	consumer::{EventConsumerInit, EventStream},
	init,
	telemetry_feed::{AddedNode, TelemetryFeed},
	telemetry_subscription::{TelemetryEvent, TelemetrySubscription},
	types::{AccountId32, SessionKeys},
	utils,
};
use polkadot_introspector_priority_channel::Receiver;
use std::str::FromStr;

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Simple telemetry feed")]
struct TelemetryOptions {
	#[clap(subcommand)]
	command: WhoisCommand,
	/// Web-Socket URLs of a relay chain node.
	#[clap(long)]
	pub ws: String,
	/// Web-Socket URL of a telemetry backend
	#[clap(long)]
	pub feed: String,
	/// Name of a chain to connect
	#[clap(long)]
	pub chain: Option<String>,
	#[clap(flatten)]
	pub verbose: init::VerbosityOptions,
	#[clap(flatten)]
	pub retry: utils::RetryOptions,
}

#[derive(Clone, Debug, Subcommand)]
enum WhoisCommand {
	Account(AccountOptions),
	Session(SessionOptions),
}

#[derive(Clone, Debug, Args)]
struct AccountOptions {
	/// SS58-formated validator's address
	pub validator: AccountId32,
}

#[derive(Clone, Debug, Args)]
struct SessionOptions {
	pub session_index: u32,
	pub validator_index: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum WhoisError {
	#[error("Validator's next keys not found")]
	NoNextKeys,
	#[error("Keys for the session with given index not found")]
	NoSessionKeys,
	#[error("Validator with given index not found")]
	NoValidator,
	#[error("Can't connect to relay chain")]
	SubxtError(SubxtWrapperError),
	#[error("Can't connect to telemetry feed")]
	TelemetryError(color_eyre::Report),
}

struct Whois {
	opts: TelemetryOptions,
}

impl Whois {
	fn new(opts: TelemetryOptions) -> color_eyre::Result<Self> {
		Ok(Self { opts })
	}

	async fn run(
		self,
		consumer_config: EventConsumerInit<TelemetryEvent>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>, WhoisError> {
		let mut executor = RequestExecutor::new(ApiClientMode::RPC, self.opts.retry.clone());
		let validator = match self.opts.command {
			WhoisCommand::Account(v) => v.validator,
			WhoisCommand::Session(v) => match executor.get_session_account_keys(&self.opts.ws, v.session_index).await {
				Ok(Some(validators)) => match validators.get(v.validator_index) {
					Some(v) => v.clone(),
					None => return Err(WhoisError::NoValidator),
				},
				Err(e) => return Err(WhoisError::SubxtError(e)),
				_ => return Err(WhoisError::NoSessionKeys),
			},
		};
		let next_keys = match executor.get_session_next_keys(&self.opts.ws, validator.clone()).await {
			Ok(Some(v)) => v,
			Err(e) => return Err(WhoisError::SubxtError(e)),
			_ => return Err(WhoisError::NoNextKeys),
		};
		let authority_key = get_authority_key(next_keys);

		let consumer_channels: Vec<Receiver<TelemetryEvent>> = consumer_config.into();
		let futures = consumer_channels
			.into_iter()
			.map(|c: Receiver<TelemetryEvent>| tokio::spawn(Self::watch(c, authority_key.clone(), validator.clone())))
			.collect();

		Ok(futures)
	}

	async fn watch(update: Receiver<TelemetryEvent>, authority_key: AccountId32, validator: AccountId32) {
		let mut count = 0_u32;
		while let Ok(TelemetryEvent::NewMessage(message)) = update.recv().await {
			if count > 0 {
				clear_last_two_lines();
			}
			count += 1;
			println!("Looking for a validator {}...\n{} telemetry messages parsed, CTRL+C to exit", validator, count);
			match message {
				TelemetryFeed::AddedNode(node) =>
					if desired_node_id(&node, authority_key.clone()) {
						println!("\n========================================\nValidator Node\n{}", node);
						std::process::exit(0);
					},
				_ => continue,
			}
		}
	}
}

fn desired_node_id(node: &AddedNode, authority_key: AccountId32) -> bool {
	if node.details.validator.is_none() {
		return false
	}

	if let Ok(node_authority_key) = AccountId32::from_str(&node.details.validator.clone().unwrap()) {
		if node_authority_key == authority_key {
			return true
		}
	};

	false
}

fn get_authority_key(keys: SessionKeys) -> AccountId32 {
	AccountId32::from(keys.grandpa.0 .0)
}

fn clear_last_two_lines() {
	print!("\x1B[2A");
	print!("\x1B[0J");
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	let opts = TelemetryOptions::parse();
	init::init_cli(&opts.verbose)?;

	let whois = Whois::new(opts.clone())?;
	let shutdown_tx = init::init_shutdown();
	let mut futures = vec![];

	let mut sub = TelemetrySubscription::new(opts.ws.clone(), opts.chain.clone());
	let consumer_init = sub.create_consumer();

	futures.extend(whois.run(consumer_init).await?);
	futures.extend(sub.run(&shutdown_tx).await?);

	init::run(futures, &shutdown_tx).await?;

	Ok(())
}
