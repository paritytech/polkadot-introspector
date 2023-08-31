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

use polkadot_introspector_essentials::types::AccountId32;
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
