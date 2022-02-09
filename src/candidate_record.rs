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

use crate::polkadot;
use crate::records_storage::StorageEntry;
use std::hash::Hash;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Stores tracking data for a candidate
#[derive(Debug)]
pub struct CandidateRecord<T: Hash> {
	/// Candidate receipt (if observed)
	candidate_receipt: Option<polkadot::runtime_types::polkadot_primitives::v1::CandidateReceipt<T>>,
	/// The time we first observed a candidate since UnixEpoch
	candidate_first_seen: Duration,
}

impl<T: Hash> Default for CandidateRecord<T> {
	fn default() -> Self {
		Self {
			candidate_receipt: None,
			candidate_first_seen: SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.expect("Clock skewed before unix epoch"),
		}
	}
}

impl<T> StorageEntry for CandidateRecord<T>
where
	T: Hash,
{
	fn get_time(&self) -> Duration {
		self.candidate_first_seen
	}
}
