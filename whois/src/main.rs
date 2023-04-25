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
use futures::future;
use log::LevelFilter;
use polkadot_introspector_essentials::{
	constants::MAX_MSG_QUEUE_SIZE,
	telemetry_feed::{AddedNode, FeedNodeId, TelemetryFeed},
	telemetry_subscription::{TelemetryEvent, TelemetrySubscription},
};
use polkadot_introspector_priority_channel::{channel as priority_channel, Receiver};
use tokio::{signal, sync::broadcast};

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
	/// Web-Socket URL of a telemetry backend
	#[clap(name = "ws", long)]
	pub url: String,
	/// Network id of the desired node to receive only events related to it
	#[clap(name = "id", long)]
	pub network_id: String,
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, action = ArgAction::Count)]
	pub verbose: u8,
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
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let mut futures = self.subscription.run(self.opts.url.clone(), shutdown_tx).await?;
		futures.push(tokio::spawn(Self::watch(self.update_channel, self.opts.network_id)));

		Ok(futures)
	}

	async fn watch(update: Receiver<TelemetryEvent>, network_id: String) {
		let mut node_id: Option<FeedNodeId> = None;

		while let Ok(TelemetryEvent::NewMessage(message)) = update.recv().await {
			match message {
				TelemetryFeed::AddedNode(v) => {
					save_node_id(&v, network_id.clone(), &mut node_id);
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

fn save_node_id(node: &AddedNode, network_id: String, holder: &mut Option<FeedNodeId>) {
	let node_network_id = node.details.network_id.clone();
	if node_network_id == Some(network_id) {
		*holder = Some(node.node_id);
	}
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;
	let opts = TelemetryOptions::parse();
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

	let shutdown_tx = init_shutdown();
	let futures =
		init_futures_with_shutdown(Telemetry::new(opts)?.run(shutdown_tx.clone()).await?, shutdown_tx.clone());
	run(futures).await?;

	Ok(())
}

fn init_shutdown() -> broadcast::Sender<()> {
	let (shutdown_tx, _) = broadcast::channel(1);
	shutdown_tx
}

fn init_futures_with_shutdown(
	mut futures: Vec<tokio::task::JoinHandle<()>>,
	shutdown_tx: broadcast::Sender<()>,
) -> Vec<tokio::task::JoinHandle<()>> {
	futures.push(tokio::spawn(on_shutdown(shutdown_tx)));
	futures
}

async fn on_shutdown(shutdown_tx: broadcast::Sender<()>) {
	signal::ctrl_c().await.unwrap();
	let _ = shutdown_tx.send(());
}

async fn run(futures: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()> {
	future::try_join_all(futures).await?;
	Ok(())
}
