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

#![cfg(test)]

use crate::parachain_block_info::ParachainBlockInfo;
use parity_scale_codec::Encode;
use polkadot_introspector_essentials::{
	api::{storage::RequestExecutor, subxt_wrapper::ApiClientMode, ApiService},
	collector::{
		candidate_record::{CandidateInclusionRecord, CandidateRecord},
		CollectorPrefixType,
	},
	metadata::{
		polkadot::runtime_types::{
			bounded_collections::bounded_vec::BoundedVec,
			polkadot_core_primitives::CandidateHash,
			polkadot_parachain::primitives::{HeadData, Id, ValidationCodeHash},
			sp_core::sr25519::{Public, Signature},
			sp_runtime::{
				generic::{digest::Digest, header::Header},
				traits::BlakeTwo256,
			},
		},
		polkadot_primitives::{
			collator_app, signed::UncheckedSigned, validator_app, AvailabilityBitfield, BackedCandidate,
			CandidateCommitments, CandidateDescriptor, CommittedCandidateReceipt, DisputeStatement,
			DisputeStatementSet, InherentData, InvalidDisputeStatementKind, ValidDisputeStatementKind, ValidatorIndex,
		},
	},
	storage::{RecordTime, RecordsStorageConfig, StorageEntry},
	types::{SubxtHrmpChannel, H256},
};
use std::{collections::BTreeMap, time::Duration};
use subxt::utils::bits::DecodedBits;

pub fn rpc_node_url() -> &'static str {
	const RPC_NODE_URL: &str = "wss://rococo-rpc.polkadot.io:443";

	if let Ok(url) = std::env::var("WS_URL") {
		return Box::leak(url.into_boxed_str())
	}

	RPC_NODE_URL
}

pub fn create_backed_candidate(para_id: u32) -> BackedCandidate<H256> {
	BackedCandidate {
		candidate: CommittedCandidateReceipt {
			descriptor: CandidateDescriptor {
				para_id: Id(para_id),
				relay_parent: H256::random(),
				collator: collator_app::Public(Public([0; 32])),
				persisted_validation_data_hash: Default::default(),
				pov_hash: Default::default(),
				erasure_root: Default::default(),
				signature: create_collator_signature(),
				para_head: Default::default(),
				validation_code_hash: ValidationCodeHash(Default::default()),
			},
			commitments: CandidateCommitments {
				upward_messages: BoundedVec(Default::default()),
				horizontal_messages: BoundedVec(Default::default()),
				new_validation_code: Default::default(),
				head_data: HeadData(Default::default()),
				processed_downward_messages: Default::default(),
				hrmp_watermark: Default::default(),
			},
		},
		validity_votes: Default::default(),
		validator_indices: DecodedBits::from_iter([true]),
	}
}

pub fn create_dispute_statement_set() -> DisputeStatementSet {
	DisputeStatementSet {
		candidate_hash: CandidateHash(H256::random()),
		session: 0,
		statements: vec![
			(
				DisputeStatement::Valid(ValidDisputeStatementKind::Explicit),
				ValidatorIndex(1),
				create_validator_signature(),
			),
			(
				DisputeStatement::Invalid(InvalidDisputeStatementKind::Explicit),
				ValidatorIndex(2),
				create_validator_signature(),
			),
			(
				DisputeStatement::Invalid(InvalidDisputeStatementKind::Explicit),
				ValidatorIndex(3),
				create_validator_signature(),
			),
		],
	}
}

pub fn create_inherent_data(para_id: u32) -> InherentData<Header<u32, BlakeTwo256>> {
	InherentData {
		bitfields: vec![UncheckedSigned {
			payload: AvailabilityBitfield(DecodedBits::from_iter([true])),
			validator_index: ValidatorIndex(1),
			signature: create_validator_signature(),
			__subxt_unused_type_params: Default::default(),
		}],
		backed_candidates: vec![create_backed_candidate(para_id)],
		disputes: vec![create_dispute_statement_set()],
		parent_header: Header {
			parent_hash: H256::random(),
			number: Default::default(),
			state_root: Default::default(),
			extrinsics_root: Default::default(),
			digest: Digest { logs: Default::default() },
			__subxt_unused_type_params: Default::default(),
		},
	}
}

pub fn create_api() -> ApiService<H256> {
	ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 4 }, ApiClientMode::RPC, Default::default())
}

pub fn create_storage() -> RequestExecutor<H256, CollectorPrefixType> {
	ApiService::new_with_prefixed_storage(
		RecordsStorageConfig { max_blocks: 4 },
		ApiClientMode::RPC,
		Default::default(),
	)
	.storage()
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
	relay_parent: H256,
	relay_parent_number: u32,
) -> CandidateRecord {
	CandidateRecord {
		candidate_inclusion: CandidateInclusionRecord {
			parachain_id: para_id,
			backed,
			included: None,
			timedout: None,
			core_idx: None,
			relay_parent,
			relay_parent_number,
		},
		candidate_first_seen: Duration::from_secs(0),
		candidate_disputed: None,
	}
}

pub fn create_para_block_info() -> ParachainBlockInfo {
	let mut info = ParachainBlockInfo::default();
	info.set_candidate(BackedCandidate {
		candidate: CommittedCandidateReceipt {
			descriptor: CandidateDescriptor {
				para_id: Id(100),
				relay_parent: Default::default(),
				collator: collator_app::Public(Public([0; 32])),
				persisted_validation_data_hash: Default::default(),
				pov_hash: Default::default(),
				erasure_root: Default::default(),
				signature: collator_app::Signature(Signature([0; 64])),
				para_head: Default::default(),
				validation_code_hash: ValidationCodeHash(Default::default()),
			},
			commitments: CandidateCommitments {
				upward_messages: BoundedVec(Default::default()),
				horizontal_messages: BoundedVec(Default::default()),
				new_validation_code: Default::default(),
				head_data: HeadData(Default::default()),
				processed_downward_messages: Default::default(),
				hrmp_watermark: Default::default(),
			},
		},
		validity_votes: vec![],
		validator_indices: DecodedBits::from_iter([true]),
	});

	info
}

pub async fn storage_write<T: Encode>(
	prefix: CollectorPrefixType,
	hash: H256,
	entry: T,
	storage: &RequestExecutor<H256, CollectorPrefixType>,
) -> color_eyre::Result<()> {
	storage
		.storage_write_prefixed(
			prefix,
			hash,
			StorageEntry::new_onchain(RecordTime::with_ts(0, Duration::from_secs(0)), entry),
		)
		.await
}

fn create_collator_signature() -> collator_app::Signature {
	collator_app::Signature(Signature([0; 64]))
}

fn create_validator_signature() -> validator_app::Signature {
	validator_app::Signature(Signature([0; 64]))
}
