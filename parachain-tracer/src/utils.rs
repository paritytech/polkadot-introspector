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

use polkadot_introspector_essentials::{
	api::subxt_wrapper::InherentData,
	metadata::polkadot_primitives::{AvailabilityBitfield, BackedCandidate, DisputeStatement, DisputeStatementSet},
	types::{AccountId32, H256},
};
use std::time::Duration;

/// Returns a time difference between optional timestamps
pub(crate) fn time_diff(lhs: Option<u64>, rhs: Option<u64>) -> Option<Duration> {
	match (lhs, rhs) {
		(Some(lhs), Some(rhs)) => Some(Duration::from_millis(lhs).saturating_sub(Duration::from_millis(rhs))),
		_ => None,
	}
}

// Examines session info (if any) and find the corresponding validator
pub(crate) fn extract_validator_address(
	session_keys: Option<&Vec<AccountId32>>,
	validator_index: u32,
) -> (u32, String) {
	if let Some(session_keys) = session_keys.as_ref() {
		if validator_index < session_keys.len() as u32 {
			let validator_identity = &session_keys[validator_index as usize];
			(validator_index, validator_identity.to_string())
		} else {
			(
				validator_index,
				format!("??? (no such validator index {}: know {} validators)", validator_index, session_keys.len()),
			)
		}
	} else {
		(validator_index, "??? (no session keys)".to_string())
	}
}

pub(crate) fn extract_validator_addresses(
	session_keys: Option<&Vec<AccountId32>>,
	indices: Vec<u32>,
) -> Vec<(u32, String)> {
	indices
		.iter()
		.map(|idx| extract_validator_address(session_keys, *idx))
		.collect()
}

pub(crate) fn extract_inherent_fields(
	data: InherentData,
) -> (Vec<AvailabilityBitfield>, Vec<BackedCandidate<H256>>, Vec<DisputeStatementSet>) {
	let bitfields = data
		.bitfields
		.into_iter()
		.map(|b| b.payload)
		.collect::<Vec<AvailabilityBitfield>>();

	(bitfields, data.backed_candidates, data.disputes)
}

pub(crate) fn backed_candidate(
	backed_candidates: Vec<BackedCandidate<H256>>,
	para_id: u32,
) -> Option<BackedCandidate<H256>> {
	backed_candidates
		.into_iter()
		.find(|candidate| candidate.candidate.descriptor.para_id.0 == para_id)
}

pub(crate) fn extract_misbehaving_validators(
	session_keys: Option<&Vec<AccountId32>>,
	info: &DisputeStatementSet,
	against_valid: bool,
) -> Vec<(u32, String)> {
	info.statements
		.iter()
		.filter(|(statement, _, _)| {
			if against_valid {
				!matches!(statement, DisputeStatement::Valid(_))
			} else {
				matches!(statement, DisputeStatement::Valid(_))
			}
		})
		.map(|(_, idx, _)| extract_validator_address(session_keys, idx.0))
		.collect()
}

pub(crate) fn extract_votes(info: &DisputeStatementSet) -> (u32, u32) {
	let voted_for = info
		.statements
		.iter()
		.filter(|(statement, _, _)| matches!(statement, DisputeStatement::Valid(_)))
		.count() as u32;
	let voted_against = info.statements.len() as u32 - voted_for;

	(voted_for, voted_against)
}

pub(crate) fn extract_availability_bits_count(bitfields: Vec<AvailabilityBitfield>, core: u32) -> u32 {
	bitfields
		.iter()
		.map(|v| v.0.as_bits().get(core as usize).expect("core index must be in the bitfield") as u32)
		.sum()
}
