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

use subxt::{ClientBuilder, DefaultConfig, DefaultExtra, EventSubscription};

#[subxt::subxt(runtime_metadata_path = "assets/rococo_metadata.scale")]
pub mod polkadot {}

use color_eyre::eyre::WrapErr;
use structopt::StructOpt;

/// Arguments for common operations
#[derive(Clone, Debug, StructOpt)]
pub(crate) struct Opts {
	/// Websockets url of a substrate node
	#[structopt(name = "url", long, default_value = "ws://localhost:9944")]
	url: String,
}
#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	env_logger::init();
	color_eyre::install()?;

	let opts = Opts::from_args();
	let api = ClientBuilder::new()
		.set_url(opts.url)
		.build()
		.await
		.context("Error connecting to substrate node")?
		.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>();
	let sub = api.client.rpc().subscribe_events().await?;
	let decoder = api.client.events_decoder();
	let mut sub = EventSubscription::<DefaultConfig>::new(sub, decoder);
	while let Some(ev) = sub.next().await {
		let ev = ev?;

		if ev.pallet == "ParasDisputes" {
			let decoded_message = match ev.variant.as_str() {
				"DisputeInitiated" => {
					format!(
						"{:?}",
						<polkadot::paras_disputes::events::DisputeInitiated as codec::Decode>::decode(
							&mut &ev.data[..]
						)?
					)
				},
				"DisputeConcluded" => {
					format!(
						"{:?}",
						<polkadot::paras_disputes::events::DisputeConcluded as codec::Decode>::decode(
							&mut &ev.data[..]
						)?
					)
				},
				"DisputeTimedOut" => {
					format!(
						"{:?}",
						<polkadot::paras_disputes::events::DisputeTimedOut as codec::Decode>::decode(
							&mut &ev.data[..]
						)?
					)
				},
				"Revert" => {
					format!(
						"{:?}",
						<polkadot::paras_disputes::events::Revert as codec::Decode>::decode(&mut &ev.data[..])?
					)
				},
				_ => "unknown".to_string(),
			};
			println!("got disputes event from a node: {}", decoded_message);
		} else {
			println!("got other event from a node: {:?}", ev);
		}
	}
	Ok(())
}
