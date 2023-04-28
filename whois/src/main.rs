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
	telemetry_feed::{AddedNode, FeedNodeId, TelemetryFeed},
	telemetry_subscription::{TelemetryEvent, TelemetrySubscription},
	types::{AccountId32, SessionKeys},
	utils,
};
use polkadot_introspector_priority_channel::{channel as priority_channel, Receiver};
use std::str::FromStr;
use tokio::sync::broadcast;

macro_rules! print_for_node_id {
	($node_id:expr, $v:expr) => {
		if $node_id == Some($v.node_id) {
			println!("{:?}\n", $v);
		}
	};
}

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Simple telemetry feed")]
pub(crate) struct TelemetryOptions {
	/// SS58-formated validator's address
	pub validator: AccountId32,
	/// Web-Socket URLs of a relay chain node.
	#[clap(name = "ws", long)]
	pub ws: String,
	/// Web-Socket URL of a telemetry backend
	#[clap(name = "feed", long)]
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
		println!("Looking for a validator {} with authority key {}.\n", self.opts.validator, authority_key);

		let mut futures = match self.subscription.run(self.opts.feed.clone(), shutdown_tx).await {
			Ok(v) => v,
			Err(e) => return Err(WhoisError::TelemetryError(e)),
		};
		futures.push(tokio::spawn(Self::watch(self.update_channel, authority_key)));

		Ok(futures)
	}

	async fn watch(update: Receiver<TelemetryEvent>, authority_key: AccountId32) {
		let mut node_id: Option<FeedNodeId> = None;

		while let Ok(TelemetryEvent::NewMessage(message)) = update.recv().await {
			match message {
				TelemetryFeed::AddedNode(v) => {
					save_node_id(&v, authority_key.clone(), &mut node_id);
					print_for_node_id!(node_id, v);
				},
				TelemetryFeed::RemovedNode(v) => print_for_node_id!(node_id, v),
				TelemetryFeed::LocatedNode(v) => print_for_node_id!(node_id, v),
				TelemetryFeed::ImportedBlock(v) => print_for_node_id!(node_id, v),
				TelemetryFeed::FinalizedBlock(v) => print_for_node_id!(node_id, v),
				TelemetryFeed::NodeStatsUpdate(v) => print_for_node_id!(node_id, v),
				TelemetryFeed::Hardware(v) => print_for_node_id!(node_id, v),
				TelemetryFeed::StaleNode(v) => print_for_node_id!(node_id, v),
				TelemetryFeed::NodeIOUpdate(v) => print_for_node_id!(node_id, v),
				_ => continue,
			}
		}
	}
}

fn save_node_id(node: &AddedNode, authority_key: AccountId32, holder: &mut Option<FeedNodeId>) {
	if node.details.validator.is_none() {
		return
	}

	if let Ok(node_authority_key) = AccountId32::from_str(&node.details.validator.clone().unwrap()) {
		if node_authority_key == authority_key {
			*holder = Some(node.node_id);
			println!("Validator's node found, subscribed to its events.\n");
		}
	};
}

fn get_authority_key(keys: SessionKeys) -> AccountId32 {
	AccountId32::from(keys.grandpa.0 .0)
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
