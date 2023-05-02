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
use polkadot_introspector_essentials::{
	api::subxt_wrapper::{RequestExecutor, SubxtWrapperError},
	constants::MAX_MSG_QUEUE_SIZE,
	init,
	telemetry_feed::{AddedNode, TelemetryFeed},
	telemetry_subscription::{TelemetryEvent, TelemetrySubscription},
	types::{AccountId32, SessionKeys},
	utils,
};
use polkadot_introspector_priority_channel::{channel as priority_channel, Receiver};
use std::str::FromStr;
use tokio::sync::broadcast;

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Simple telemetry feed")]
pub(crate) struct TelemetryOptions {
	/// SS58-formated validator's address
	pub validator: AccountId32,
	/// Web-Socket URLs of a relay chain node.
	#[clap(long)]
	pub ws: String,
	/// Web-Socket URL of a telemetry backend
	#[clap(long)]
	pub feed: String,
	#[clap(flatten)]
	pub verbose: init::VerbosityOptions,
	#[clap(flatten)]
	pub retry: utils::RetryOptions,
}

#[derive(Debug, thiserror::Error)]
pub enum WhoisError {
	#[error("Validator's next keys not found")]
	NoNextKeys,
	#[error("Can't connect to relay chain")]
	SubxtError(SubxtWrapperError),
	#[error("Can't connect to telemetry feed")]
	TelemetryError(color_eyre::Report),
}

pub(crate) struct Telemetry {
	opts: TelemetryOptions,
	subscription: TelemetrySubscription,
	update_channel: Receiver<TelemetryEvent>,
}

impl Telemetry {
	pub(crate) fn new(opts: TelemetryOptions) -> color_eyre::Result<Self> {
		let (update_tx, update_rx) = priority_channel(MAX_MSG_QUEUE_SIZE);
		Ok(Self { opts, subscription: TelemetrySubscription::new(vec![update_tx]), update_channel: update_rx })
	}

	pub(crate) async fn run(
		self,
		shutdown_tx: broadcast::Sender<()>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>, WhoisError> {
		let mut executor = RequestExecutor::new(self.opts.retry.clone());
		let session_keys = match executor.get_session_next_keys(&self.opts.ws, self.opts.validator.clone()).await {
			Ok(Some(v)) => v,
			Err(e) => return Err(WhoisError::SubxtError(e)),
			_ => return Err(WhoisError::NoNextKeys),
		};
		let authority_key = get_authority_key(session_keys);
		let mut futures = match self.subscription.run(self.opts.feed.clone(), shutdown_tx).await {
			Ok(v) => v,
			Err(e) => return Err(WhoisError::TelemetryError(e)),
		};
		futures.push(tokio::spawn(Self::watch(self.update_channel, authority_key, self.opts.validator.clone())));

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

	return false
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

	let shutdown_tx = init::init_shutdown();
	let futures =
		init::init_futures_with_shutdown(Telemetry::new(opts)?.run(shutdown_tx.clone()).await?, shutdown_tx.clone());
	init::run(futures).await?;

	Ok(())
}
