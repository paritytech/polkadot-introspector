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

use super::records_storage::StorageEntry;
use crate::polkadot::runtime_types::polkadot_primitives::v1 as polkadot_rt_primitives;
use serde::{
	ser::{SerializeStruct, Serializer},
	Deserialize, Serialize,
};
use serde_bytes::Bytes;

use std::{
	hash::Hash,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Tracks candidate inclusion as seen by a node(s)
#[derive(Debug, Serialize, Deserialize)]
pub struct CandidateInclusion<T: Hash> {
	/// Parachain id (must be known if we have observed a candidate receipt)
	pub parachain_id: Option<u32>,
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
		Self { parachain_id: None, baked: None, included: None, timedout: None, core_idx: None, relay_parent: None }
	}
}

/// Outcome of the dispute
#[derive(Debug, Copy, Clone, Serialize)]
pub enum DisputeOutcome {
	/// Dispute has not been concluded yet
	InProgress,
	/// Dispute has been concluded as invalid
	Invalid,
	/// Dispute has beed concluded as valid
	Agreed,
	/// Dispute resolution has timed out
	TimedOut,
}

impl Default for DisputeOutcome {
	fn default() -> Self {
		DisputeOutcome::InProgress
	}
}

/// Outcome of the dispute + timestamp
#[derive(Debug, Default, Clone, Serialize)]
pub struct DisputeResult {
	/// The current outcome
	pub outcome: DisputeOutcome,
	/// Timestamp of a conclusion
	pub concluded_timestamp: Duration,
}

/// Tracks candidate disputes as seen by a node(s)
#[derive(Debug, Default, Clone, Serialize)]
pub struct CandidateDisputed {
	/// When do we observe this dispute
	pub disputed: Duration,
	/// Result of a dispute
	pub concluded: Option<DisputeResult>,
}

impl<T> Serialize for polkadot_rt_primitives::CandidateDescriptor<T>
where
	T: Hash + Serialize,
{
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let mut state = serializer.serialize_struct("CandidateDescriptor", 9)?;
		state.serialize_field("para_id", &self.para_id.0)?;
		state.serialize_field("relay_parent", &self.relay_parent)?;
		state.serialize_field("collator", &self.collator.0 .0)?;
		state.serialize_field("persisted_validation_data_hash", &self.persisted_validation_data_hash)?;
		state.serialize_field("pov_hash", &self.pov_hash)?;
		state.serialize_field("erasure_root", &self.erasure_root)?;
		state.serialize_field("signature", Bytes::new(&self.signature.0 .0))?;
		state.serialize_field("para_head", &self.para_head)?;
		state.serialize_field("validation_code_hash", &self.validation_code_hash.0)?;
		state.end()
	}
}

impl<T> Serialize for polkadot_rt_primitives::CandidateReceipt<T>
where
	T: Hash + Serialize,
{
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let mut state = serializer.serialize_struct("CandidateReceipt", 2)?;
		state.serialize_field("descriptor", &self.descriptor)?;
		state.serialize_field("commitments_hash", &self.commitments_hash)?;
		state.end()
	}
}

/// Stores tracking data for a candidate
#[derive(Debug, Serialize)]
pub struct CandidateRecord<T: Hash + Serialize> {
	/// Candidate receipt (if observed)
	pub candidate_receipt: Option<polkadot_rt_primitives::CandidateReceipt<T>>,
	/// The time we first observed a candidate since UnixEpoch
	pub candidate_first_seen: Duration,
	/// Inclusion data
	pub candidate_inclusion: CandidateInclusion<T>,
	/// Dispute data
	pub candidate_disputed: Option<CandidateDisputed>,
}

impl<T: Hash + Serialize> Default for CandidateRecord<T> {
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
	T: Hash + Serialize,
{
	fn get_time(&self) -> Duration {
		self.candidate_first_seen
	}
}

impl<T> CandidateRecord<T>
where
	T: Hash + Serialize,
{
	/// Returns if a candidate has been disputed
	#[allow(dead_code)]
	pub fn is_disputed(&self) -> bool {
		self.candidate_disputed.is_some()
	}

	/// Returns inclusion time for a candidate
	#[allow(dead_code)]
	pub fn inclusion_time(&self) -> Option<Duration> {
		match (self.candidate_inclusion.baked, self.candidate_inclusion.included) {
			(Some(baked), Some(included)) => included.checked_sub(baked),
			_ => None,
		}
	}

	/// Returns dispute resolution time
	#[allow(dead_code)]
	pub fn dispute_resolution_time(&self) -> Option<Duration> {
		self.candidate_disputed.as_ref().and_then(|disp| {
			let concluded = disp.concluded.as_ref()?;
			concluded.concluded_timestamp.checked_sub(disp.disputed)
		})
	}

	/// Returns a relay parent for a specific candidate
	#[allow(dead_code)]
	pub fn relay_parent(&self) -> Option<&T> {
		let receipt = &self.candidate_receipt.as_ref()?;
		let descriptor = &receipt.descriptor;
		Some(&descriptor.relay_parent)
	}

	pub fn parachain_id(&self) -> Option<u32> {
		self.candidate_inclusion.parachain_id
	}
}
