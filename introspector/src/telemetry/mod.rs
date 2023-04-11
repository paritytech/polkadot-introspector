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
use priority_channel::Receiver;
use subxt::utils::H256;
use tokio::sync::broadcast::Sender as BroadcastSender;

use crate::core::{AddedNode, FeedNodeId, TelemetryEvent, TelemetryFeed, TelemetrySubscription, MAX_MSG_QUEUE_SIZE};

macro_rules! print_for_node_id {
	($node_id:expr, $v:expr) => {
		if $node_id == Some($v.node_id) {
			println!("{:?}", $v);
		}
	};
}

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct TelemetryOptions {
	/// Web-Socket URL of a telemetry backend
	#[clap(name = "ws", long)]
	pub url: String,
	/// Chain's genesis hash (telemetry can collect events from many chains, so we need to specify the chain via its genesis hash)
	#[clap(name = "chain", long)]
	pub chain_hash: H256,
	/// Network id of the desired node to receive only events related to it
	#[clap(name = "id", long)]
	pub network_id: String,
}

pub(crate) struct Telemetry {
	opts: TelemetryOptions,
	subscription: TelemetrySubscription,
	update_channel: Receiver<TelemetryEvent>,
}

impl Telemetry {
	pub(crate) fn new(opts: TelemetryOptions) -> color_eyre::Result<Self> {
		let (update_tx, update_rx) = priority_channel::channel(MAX_MSG_QUEUE_SIZE);
		Ok(Self { opts, subscription: TelemetrySubscription::new(vec![update_tx]), update_channel: update_rx })
	}

	pub(crate) async fn run(
		self,
		shutdown_tx: BroadcastSender<()>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let mut futures = self
			.subscription
			.run(self.opts.url.clone(), self.opts.chain_hash, shutdown_tx)
			.await?;
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