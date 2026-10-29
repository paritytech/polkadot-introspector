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
//

use crate::types::{H256, Header, PolkadotHash};
use parity_scale_codec::{Decode, Encode};
use serde::Serialize;

#[derive(Debug)]
pub enum ChainEvent {
	/// New best relay chain head
	NewBestHead((H256, Header)),
	/// New finalized relay chain head
	NewFinalizedHead((H256, Header)),
	/// Dispute for a specific candidate hash
	DisputeInitiated(SubxtDispute),
	/// Conclusion for a dispute
	DisputeConcluded(SubxtDispute, SubxtDisputeResult),
	/// Backing, inclusion, time out for a parachain candidate
	CandidateChanged(Box<SubxtCandidateEvent>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubxtCandidateEventType {
	/// Candidate has been backed
	Backed,
	/// Candidate has been included
	Included,
	/// Candidate has been timed out
	TimedOut,
}
/// A structure that helps to deal with the candidate events decoding some of
/// the important fields there
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubxtCandidateEvent {
	/// Result of candidate receipt hashing
	pub candidate_hash: PolkadotHash,
	/// Relay parent from the candidate descriptor
	pub relay_parent: H256,
	/// The parachain id
	pub parachain_id: u32,
	/// The event type
	pub event_type: SubxtCandidateEventType,
	/// Core index
	pub core_idx: u32,
}

/// A helper structure to keep track of a dispute and it's relay parent
#[derive(Debug, Clone, Encode, Decode)]
pub struct SubxtDispute {
	/// Relay chain block where a dispute has taken place
	pub relay_parent_block: PolkadotHash,
	/// Specific candidate being disputed about
	pub candidate_hash: PolkadotHash,
}

/// Dispute result as seen by subxt event
#[derive(Debug, Clone, Copy, Serialize, Decode, Encode, PartialEq, Eq, Default)]
pub enum SubxtDisputeResult {
	/// Dispute outcome is valid
	#[default]
	Valid,
	/// Dispute outcome is invalid
	Invalid,
	/// Dispute has been timed out
	TimedOut,
}
