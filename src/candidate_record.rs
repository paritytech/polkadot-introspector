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

/// Tracks candidate inclusion as seen by a node(s)
#[derive(Debug)]
pub struct CandidateInclusion<T: Hash> {
	/// Time when a candidate has been baked
	pub baked: Option<Duration>,
	/// Time when a candidate has been included
	pub included: Option<Duration>,
	/// Time when a candidate has been timed out
	pub timedout: Option<Duration>,
	/// Observed core index
	pub core_idx: Option<u32>,
	/// Relay parent
	pub relay_parent: Option<T>,
}

impl<T: Hash> Default for CandidateInclusion<T> {
	fn default() -> Self {
		Self { baked: None, included: None, timedout: None, core_idx: None, relay_parent: None }
	}
}

/// Outcome of the dispute
#[derive(Debug, Copy, Clone)]
pub enum DisputeOutcome {
	/// Dispute has not been concluded yet
	DisputeInProgress,
	/// Dispute has been concluded as invalid
	DisputeInvalid,
	/// Dispute has beed concluded as valid
	DisputeAgreed,
	/// Dispute resolution has timed out
	DisputeTimedOut,
}

impl Default for DisputeOutcome {
	fn default() -> Self {
		DisputeOutcome::DisputeInProgress
	}
}

/// Outcome of the dispute + timestamp
#[derive(Debug, Default, Clone)]
pub struct DisputeResult {
	/// The current outcome
	pub outcome: DisputeOutcome,
	/// Timestamp of a conclusion
	pub concluded_timestamp: Duration,
}

/// Tracks candidate disputes as seen by a node(s)
#[derive(Debug, Default, Clone)]
pub struct CandidateDisputed {
	/// When do we observe this dispute
	pub disputed: Duration,
	/// Result of a dispute
	pub concluded: Option<DisputeResult>,
}

/// Stores tracking data for a candidate
#[derive(Debug)]
pub struct CandidateRecord<T: Hash> {
	/// Candidate receipt (if observed)
	pub candidate_receipt: Option<polkadot::runtime_types::polkadot_primitives::v1::CandidateReceipt<T>>,
	/// The time we first observed a candidate since UnixEpoch
	pub candidate_first_seen: Duration,
	/// Inclusion data
	pub candidate_inclusion: CandidateInclusion<T>,
	/// Dispute data
	pub candidate_disputed: Option<CandidateDisputed>,
}

impl<T: Hash> Default for CandidateRecord<T> {
	fn default() -> Self {
		Self {
			candidate_receipt: None,
			candidate_first_seen: SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.expect("Clock skewed before unix epoch"),
			candidate_inclusion: Default::default(),
			candidate_disputed: None,
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
