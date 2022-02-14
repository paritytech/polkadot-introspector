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

use color_eyre::eyre::{eyre, WrapErr};
use log::LevelFilter;
use log::{info, warn};
use sp_core::H256;
use std::sync::Arc;
use std::sync::Mutex;
use structopt::StructOpt;
use subxt::{ClientBuilder, DefaultConfig, DefaultExtra, EventSubscription};

#[subxt::subxt(runtime_metadata_path = "assets/rococo_metadata.scale")]
pub mod polkadot {}

mod candidate_record;
mod event_handler;
mod records_storage;

use crate::candidate_record::CandidateRecord;
use crate::event_handler::EventsHandler;
use crate::records_storage::{RecordsStorage, RecordsStorageConfig};

/// Arguments for common operations
#[derive(Clone, Debug, StructOpt)]
pub(crate) struct Opts {
	/// Websockets url of a substrate node
	#[structopt(name = "url", long, default_value = "ws://localhost:9944")]
	url: String,
	/// Maximum candidates to store
	#[structopt(name = "max-candidates", long)]
	max_candidates: Option<usize>,
	/// Maximum age of candidates to preserve (default: 1 day)
	#[structopt(name = "max-ttl", long, default_value = "86400")]
	max_ttl: usize,
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[structopt(short = "v", long, parse(from_occurrences))]
	verbose: i8,
}

impl From<&Opts> for RecordsStorageConfig {
	fn from(opts: &Opts) -> RecordsStorageConfig {
		RecordsStorageConfig { max_ttl: Some(opts.max_ttl), max_records: opts.max_candidates }
	}
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;

	let opts = Opts::from_args();
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
	let records_storage = Arc::new(Mutex::new(RecordsStorage::<H256, CandidateRecord<H256>>::new((&opts).into())));

	// TODO: might be useful to process multiple nodes in different tasks
	let api = ClientBuilder::new()
		.set_url(opts.url.clone())
		.build()
		.await
		.context("Error connecting to substrate node")?
		.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>();
	info!("Connected to a substrate node {}", &opts.url);
	let mut events_handler = EventsHandler::builder().storage(records_storage.clone()).build();

	let sub = api.client.rpc().subscribe_events().await?;
	let decoder = api.client.events_decoder();
	let mut sub = EventSubscription::<DefaultConfig>::new(sub, decoder);
	while let Some(ev_ctx) = sub.next_context().await {
		let ev_ctx = ev_ctx?;
		events_handler
			.handle_runtime_event(&ev_ctx.event, &ev_ctx.block_hash)
			.map_err(|e| warn!("cannot handle event: {:?}", e));
	}
	Ok(())
}
