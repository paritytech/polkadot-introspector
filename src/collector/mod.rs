use log::{info, warn, LevelFilter};
use sp_core::H256;
use std::sync::{Arc, Mutex};
use subxt::{ClientBuilder, DefaultConfig, DefaultExtra, EventSubscription};

mod candidate_record;
mod event_handler;
mod records_storage;

use candidate_record::*;
use color_eyre::eyre::WrapErr;
use event_handler::*;
use records_storage::*;

use crate::{polkadot, CollectorOptions};

impl From<&CollectorOptions> for RecordsStorageConfig {
	fn from(opts: &CollectorOptions) -> RecordsStorageConfig {
		RecordsStorageConfig { max_ttl: Some(opts.max_ttl), max_records: opts.max_candidates }
	}
}

pub(crate) async fn run(opts: CollectorOptions) -> color_eyre::Result<()> {
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
		let _ = events_handler
			.handle_runtime_event(&ev_ctx.event, &ev_ctx.block_hash)
			.map_err(|e| warn!("cannot handle event: {:?}", e));
	}
	Ok(())
}
