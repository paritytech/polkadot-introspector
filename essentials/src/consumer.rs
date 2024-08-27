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
//
//! Event consumer traits and types. Abstracts on-chain/off-chain event streams and
//! APIs for RPC nodes and internal storage.
use crate::init::Shutdown;
use async_trait::async_trait;
use polkadot_introspector_priority_channel::Receiver;
use tokio::sync::broadcast::Sender as BroadcastSender;

#[async_trait]
pub trait EventStream {
	type Event;

	/// Create a consumer config
	fn create_consumer(&mut self) -> EventConsumerInit<Self::Event>;

	/// Prepare futures to join.
	async fn run(
		&self,
		shutdown_tx: &BroadcastSender<Shutdown>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>>;
}

#[derive(Debug)]
pub struct EventConsumerInit<Event> {
	// One per ws connection.
	update_channels: Vec<Receiver<Event>>,
}

impl<Event> From<EventConsumerInit<Event>> for Vec<Receiver<Event>> {
	fn from(event: EventConsumerInit<Event>) -> Self {
		event.update_channels
	}
}

impl<Event> EventConsumerInit<Event> {
	pub fn new(update_channels: Vec<Receiver<Event>>) -> Self {
		Self { update_channels }
	}
}
