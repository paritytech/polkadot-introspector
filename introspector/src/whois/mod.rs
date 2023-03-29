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

use std::time::Duration;

use clap::Parser;
use priority_channel::{Receiver, TryRecvError};
use subxt::utils::H256;
use tokio::sync::broadcast::Sender as BroadcastSender;

use crate::core::{TelemetryEvent, TelemetrySubscription, MAX_MSG_QUEUE_SIZE};

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct WhoisOptions {
	/// Web-Socket URL of a telemetry backend
	#[clap(name = "ws", long)]
	pub url: String,
	// Chain's genesis hash
	#[clap(name = "chain", long)]
	pub chain_hash: H256,
}

pub(crate) struct Whois {
	opts: WhoisOptions,
	subscription: TelemetrySubscription,
	update_channel: Receiver<TelemetryEvent>,
}

impl Whois {
	pub(crate) fn new(opts: WhoisOptions) -> color_eyre::Result<Self> {
		let (update_tx, update_rx) = priority_channel::channel(MAX_MSG_QUEUE_SIZE);
		Ok(Self { opts, subscription: TelemetrySubscription::new(vec![update_tx]), update_channel: update_rx })
	}

	pub(crate) async fn run(
		self,
		shutdown_tx: BroadcastSender<()>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let mut futures = self
			.subscription
			.run(self.opts.url.clone(), self.opts.chain_hash.clone(), shutdown_tx)
			.await?;
		futures.push(tokio::spawn(Self::watch(self.update_channel)));

		Ok(futures)
	}

	async fn watch(update: Receiver<TelemetryEvent>) {
		loop {
			match update.try_recv() {
				Ok(event) => {
					println!("{:?}", event);
				},
				Err(TryRecvError::Closed) => break,
				Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(1000)).await,
			}
		}
	}
}
