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

use crate::core::polkadot::runtime_types::polkadot_primitives::v2 as polkadot_rt_primitives;
use serde::{
	ser::{SerializeStruct, Serializer},
	Deserialize, Serialize,
};
use serde_bytes::Bytes;

use crate::core::SubxtDisputeResult;
use codec::{Decode, Encode};
use std::{hash::Hash, time::Duration};
use subxt::utils::H256;

/// Tracks candidate inclusion as seen by a node(s)
#[derive(Debug, Serialize, Deserialize, Encode, Decode, Clone)]
pub struct CandidateInclusionRecord<T: Encode + Decode + Clone> {
	/// Parachain id (must be known if we have observed a candidate receipt)
	pub parachain_id: u32,
	/// Relay parent block number when a candidate was backed
	pub backed: u32,
	/// Relay parent block number when a candidate was included
	pub included: Option<u32>,
	/// Relay parent block number when a candidate was timed out
	pub timedout: Option<u32>,
	/// Observed core index
	pub core_idx: Option<u32>,
	/// Stated relay parent
	pub relay_parent: T,
}

/// Outcome of the dispute + timestamp
#[derive(Debug, Clone, Serialize, Decode, Encode)]
pub struct DisputeResult {
	/// The current outcome
	pub outcome: SubxtDisputeResult,
	/// Relay block number of a conclusion
	pub concluded_block: u32,
}

/// Tracks candidate disputes as seen by a node(s)
#[derive(Debug, Clone, Serialize, Decode, Encode)]
pub struct CandidateDisputed {
	/// When do we observe this dispute (relay block number)
	pub disputed: u32,
	/// Result of a dispute
	pub concluded: Option<DisputeResult>,
}

impl<T> Serialize for polkadot_rt_primitives::CandidateDescriptor<T>
where
	T: Hash + Serialize + Decode + Encode,
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
	T: Hash + Serialize + Decode + Encode,
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
#[derive(Debug, Serialize, Encode, Decode)]
pub struct CandidateRecord {
	/// The relay block number when we first observed a candidate
	pub candidate_first_seen: Duration,
	/// Inclusion data
	pub candidate_inclusion: CandidateInclusionRecord<H256>,
	/// Dispute data
	pub candidate_disputed: Option<CandidateDisputed>,
}

impl CandidateRecord {
	/// Returns if a candidate has been disputed
	#[allow(dead_code)]
	pub fn is_disputed(&self) -> bool {
		self.candidate_disputed.is_some()
	}

	/// Returns inclusion time for a candidate
	#[allow(dead_code)]
	pub fn inclusion_time(&self) -> Option<u32> {
		match (&self.candidate_inclusion.backed, self.candidate_inclusion.included.as_ref()) {
			(backed, Some(included)) => included.checked_sub(*backed),
			_ => None,
		}
	}

	/// Returns dispute resolution time
	#[allow(dead_code)]
	pub fn dispute_resolution_time(&self) -> Option<u32> {
		self.candidate_disputed.as_ref().and_then(|disp| {
			let concluded = disp.concluded.as_ref()?;
			concluded.concluded_block.checked_sub(disp.disputed)
		})
	}

	pub fn parachain_id(&self) -> u32 {
		self.candidate_inclusion.parachain_id
	}
}
