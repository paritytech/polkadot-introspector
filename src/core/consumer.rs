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

use super::Request;
use async_trait::async_trait;
use tokio::sync::mpsc::{Receiver, Sender};

#[async_trait]
pub trait EventStream {
	type Event;

	fn create_consumer(&mut self) -> EventConsumerInit<Self::Event>;
	/// Run the main event loop.
	async fn run(self, tasks: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()>;
}

#[derive(Debug)]
pub struct EventConsumerInit<Event> {
	// One per ws connection.
	update_channels: Vec<Receiver<Event>>,
	to_api: Sender<Request>,
}

impl<Event> Into<(Vec<Receiver<Event>>, Sender<Request>)> for EventConsumerInit<Event> {
	fn into(self) -> (Vec<Receiver<Event>>, Sender<Request>) {
		(self.update_channels, self.to_api)
	}
}

impl<Event> EventConsumerInit<Event> {
	pub fn new(update_channels: Vec<Receiver<Event>>, to_api: Sender<Request>) -> Self {
		Self { update_channels, to_api }
	}
}
