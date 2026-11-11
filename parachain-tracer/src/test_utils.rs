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

use crate::parachain_block_info::ParachainBlockInfo;
use parity_scale_codec::Encode;
use polkadot_introspector_essentials::{
	api::{ApiService, api_client::ApiClientMode, executor, storage},
	collector::{
		CollectorPrefixType,
		candidate_record::{CandidateInclusionRecord, CandidateRecord},
	},
	init,
	storage::{RecordTime, RecordsStorageConfig, StorageEntry},
	types::{
		AvailabilityBitfield, DisputeStatement, DisputeStatementSet, H256, InherentData, InvalidDisputeStatementKind,
		SubxtHrmpChannel, ValidDisputeStatementKind, ValidatorIndex, hash_of,
	},
	utils::RetryOptions,
};
use std::{collections::BTreeMap, time::Duration};

pub fn rpc_node_url() -> &'static str {
	const RPC_NODE_URL: &str = "wss://rococo-rpc.polkadot.io:443";

	if let Ok(url) = std::env::var("WS_URL") {
		return Box::leak(url.into_boxed_str());
	}

	RPC_NODE_URL
}

pub fn create_dispute_statement_set() -> DisputeStatementSet {
	DisputeStatementSet {
		candidate_hash: H256::random(),
		session: 0,
		statements: vec![
			(DisputeStatement::Valid(ValidDisputeStatementKind::Explicit), ValidatorIndex(1)),
			(DisputeStatement::Invalid(InvalidDisputeStatementKind::Explicit), ValidatorIndex(2)),
			(DisputeStatement::Invalid(InvalidDisputeStatementKind::Explicit), ValidatorIndex(3)),
		],
	}
}

pub fn create_inherent_data() -> InherentData {
	InherentData {
		bitfields: vec![AvailabilityBitfield::from_iter([true])],
		disputes: vec![create_dispute_statement_set()],
	}
}

pub async fn create_executor() -> executor::RequestExecutor {
	let shutdown_tx = init::init_shutdown();
	executor::RequestExecutor::build(rpc_node_url(), ApiClientMode::RPC, &RetryOptions::default(), &shutdown_tx)
		.await
		.unwrap()
}

pub async fn create_storage() -> storage::RequestExecutor<H256, CollectorPrefixType> {
	ApiService::new_with_prefixed_storage(RecordsStorageConfig { max_blocks: 4 }, create_executor().await).storage()
}

pub fn create_hrmp_channels() -> BTreeMap<u32, SubxtHrmpChannel> {
	let mut channels = BTreeMap::new();
	channels.insert(100, SubxtHrmpChannel { total_size: 1, ..Default::default() });
	channels.insert(200, SubxtHrmpChannel { total_size: 0, ..Default::default() });

	channels
}

pub fn create_candidate_record(
	para_id: u32,
	backed: u32,
	included: Option<u32>,
	relay_parent: H256,
	relay_parent_number: u32,
) -> CandidateRecord {
	CandidateRecord {
		candidate_inclusion: CandidateInclusionRecord {
			parachain_id: para_id,
			backed,
			included,
			timedout: None,
			core_idx: 0,
			relay_parent,
			relay_parent_number,
		},
		candidate_first_seen: Duration::from_secs(0),
		candidate_disputed: None,
	}
}

pub fn candidate_hash(para_id: u32) -> H256 {
	hash_of(&(para_id, H256::random()))
}

pub fn create_para_block_info(para_id: u32) -> ParachainBlockInfo {
	ParachainBlockInfo::new(candidate_hash(para_id), 0)
}

pub async fn storage_write<T: Encode>(
	prefix: CollectorPrefixType,
	hash: H256,
	entry: T,
	storage: &storage::RequestExecutor<H256, CollectorPrefixType>,
) -> color_eyre::Result<()> {
	storage
		.storage_write_prefixed(
			prefix,
			hash,
			StorageEntry::new_onchain(RecordTime::with_ts(0, Duration::from_secs(0)), entry),
		)
		.await
}
