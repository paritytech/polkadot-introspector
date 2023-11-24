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
	metadata::polkadot_primitives::{AvailabilityBitfield, BackedCandidate, DisputeStatement, DisputeStatementSet},
	types::{AccountId32, InherentData, Timestamp, H256},
};
use std::time::Duration;

/// Returns a time difference between optional timestamps
pub(crate) fn time_diff(lhs: Option<u64>, rhs: Option<u64>) -> Option<Duration> {
	Some(Duration::from_millis(lhs?).saturating_sub(Duration::from_millis(rhs?)))
}

#[cfg(test)]
mod test_time_diff {
	use super::*;
	use std::time::Duration;

	#[test]
	fn test_returns_diff_if_both_are_some() {
		assert_eq!(time_diff(Some(1), Some(0)), Some(Duration::from_millis(1)));
	}

	#[test]
	fn test_returns_none_if_one_is_none() {
		assert_eq!(time_diff(Some(1), None), None);
		assert_eq!(time_diff(None, Some(1)), None);
	}

	#[test]
	fn test_returns_none_if_both_are_none() {
		assert_eq!(time_diff(None, None), None);
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

#[cfg(test)]
mod test_extract_validator_address {
	use super::*;
	use subxt::utils::AccountId32;

	#[test]
	fn test_returns_a_placeholder_if_no_session_keys() {
		assert_eq!(extract_validator_address(None, 42), (42, "??? (no session keys)".to_string()));
	}

	#[test]
	fn test_returns_a_placeholder_if_no_validator_in_session_keys() {
		let keys = vec![];
		assert_eq!(
			extract_validator_address(Some(&keys), 42),
			(42, "??? (no such validator index 42: know 0 validators)".to_string())
		);
	}

	#[test]
	fn test_otherwise_returns_the_identity() {
		let keys = vec![AccountId32([0; 32])];
		assert_eq!(
			extract_validator_address(Some(&keys), 0),
			(0, "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM".to_string())
		);
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

#[cfg(test)]
mod test_extract_validator_addresses {
	use super::*;
	use subxt::utils::AccountId32;

	#[test]
	fn test_returns_identities() {
		let keys = vec![AccountId32([0; 32])];
		assert_eq!(
			extract_validator_addresses(Some(&keys), vec![0]),
			vec![(0, "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM".to_string())]
		);
	}
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

#[cfg(test)]
mod test_extract_inherent_fields {
	use super::*;
	use crate::test_utils::create_inherent_data;

	#[test]
	fn test_returns_fields() {
		let (bitfields, backed_candidates, disputes) = extract_inherent_fields(create_inherent_data(100));

		println!("{:?}", matches!(bitfields.first().unwrap(), AvailabilityBitfield(_)));

		assert!(matches!(bitfields.first().unwrap(), AvailabilityBitfield(_)));
		assert!(matches!(backed_candidates.first().unwrap(), BackedCandidate { .. }));
		assert!(matches!(disputes.first().unwrap(), DisputeStatementSet { .. }));
	}
}

pub(crate) fn backed_candidate(
	backed_candidates: Vec<BackedCandidate<H256>>,
	para_id: u32,
) -> Option<BackedCandidate<H256>> {
	backed_candidates
		.into_iter()
		.find(|candidate| candidate.candidate.descriptor.para_id.0 == para_id)
}

#[cfg(test)]
mod test_backed_candidate {
	use super::*;
	use crate::test_utils::create_backed_candidate;

	#[test]
	fn test_returns_a_candidate() {
		let found = backed_candidate(vec![create_backed_candidate(100), create_backed_candidate(200)], 100).unwrap();

		assert_eq!(found.candidate.descriptor.para_id.0, 100);
	}
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

#[cfg(test)]
mod test_extract_misbehaving_validators {
	use super::*;
	use crate::test_utils::create_dispute_statement_set;

	#[test]
	fn test_returns_misbehaving_validators() {
		assert_eq!(
			extract_misbehaving_validators(None, &create_dispute_statement_set(), true),
			vec![(2, "??? (no session keys)".to_string()), (3, "??? (no session keys)".to_string())]
		)
	}
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

#[cfg(test)]
mod test_extract_votes {
	use super::*;
	use crate::test_utils::create_dispute_statement_set;

	#[test]
	fn test_returns_votes() {
		assert_eq!(extract_votes(&create_dispute_statement_set()), (1, 2));
	}
}

pub(crate) fn extract_availability_bits_count(bitfields: Vec<AvailabilityBitfield>, core: u32) -> u32 {
	bitfields
		.iter()
		.map(|v| v.0.as_bits().get(core as usize).expect("core index must be in the bitfield") as u32)
		.sum()
}

#[cfg(test)]
mod test_extract_availability_bits_count {
	use super::*;
	use subxt::utils::bits::DecodedBits;

	#[test]
	fn test_counts_availability_bits() {
		assert_eq!(
			extract_availability_bits_count(vec![AvailabilityBitfield(DecodedBits::from_iter([true, false, true]))], 0),
			1
		);
	}
}

/// Format the current block inherent timestamp.
pub(crate) fn format_ts(duration: Duration, current_block_ts: Timestamp) -> String {
	let dt = time::OffsetDateTime::from_unix_timestamp_nanos(current_block_ts as i128 * 1_000_000).unwrap();
	format!(
		"{} +{}",
		dt.format(&time::format_description::well_known::Iso8601::DEFAULT)
			.expect("Invalid datetime format"),
		format_args!("{}ms", duration.as_millis())
	)
}

#[cfg(test)]
mod test_format_ts {
	use super::*;

	#[test]
	fn test_formats_ts() {
		assert_eq!(format_ts(Duration::from_millis(5999), 1694002836000), "2023-09-06T12:20:36.000000000Z +5999ms")
	}
}
