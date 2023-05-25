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

use crate::metadata::{
	polkadot::{
		para_inclusion::events::{CandidateBacked, CandidateIncluded, CandidateTimedOut},
		paras_disputes::events::{DisputeConcluded, DisputeInitiated},
	},
	polkadot_primitives::CandidateDescriptor,
};
use codec::{Decode, Encode};
use color_eyre::{eyre::eyre, Result};
use serde::Serialize;
use subxt::{
	config::{substrate::BlakeTwo256, Hasher},
	PolkadotConfig,
};

#[derive(Debug)]
pub enum ChainEvent {
	/// New best relay chain head
	NewBestHead(<PolkadotConfig as subxt::Config>::Hash),
	/// New finalized relay chain head
	NewFinalizedHead(<PolkadotConfig as subxt::Config>::Hash),
	/// Dispute for a specific candidate hash
	DisputeInitiated(SubxtDispute),
	/// Conclusion for a dispute
	DisputeConcluded(SubxtDispute, SubxtDisputeResult),
	/// Backing, inclusion, time out for a parachain candidate
	CandidateChanged(Box<SubxtCandidateEvent>),
	/// Anything undecoded
	RawEvent(<PolkadotConfig as subxt::Config>::Hash, subxt::events::EventDetails),
}

#[derive(Debug)]
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
#[derive(Debug)]
pub struct SubxtCandidateEvent {
	/// Result of candidate receipt hashing
	pub candidate_hash: <PolkadotConfig as subxt::Config>::Hash,
	/// Full candidate receipt if needed
	pub candidate_descriptor: CandidateDescriptor<<PolkadotConfig as subxt::Config>::Hash>,
	/// The parachain id
	pub parachain_id: u32,
	/// The event type
	pub event_type: SubxtCandidateEventType,
}

/// A helper structure to keep track of a dispute and it's relay parent
#[derive(Debug, Clone, Encode, Decode)]
pub struct SubxtDispute {
	/// Relay chain block where a dispute has taken place
	pub relay_parent_block: <PolkadotConfig as subxt::Config>::Hash,
	/// Specific candidate being disputed about
	pub candidate_hash: <PolkadotConfig as subxt::Config>::Hash,
}

/// Dispute result as seen by subxt event
#[derive(Debug, Clone, Copy, Serialize, Decode, Encode, PartialEq, Eq)]
pub enum SubxtDisputeResult {
	/// Dispute outcome is valid
	Valid,
	/// Dispute outcome is invalid
	Invalid,
	/// Dispute has been timed out
	TimedOut,
}

pub async fn decode_chain_event(
	block_hash: <PolkadotConfig as subxt::Config>::Hash,
	event: subxt::events::EventDetails,
) -> Result<ChainEvent> {
	use crate::metadata::polkadot::runtime_types::polkadot_runtime_parachains::disputes::DisputeResult as RuntimeDisputeResult;

	let subxt_event = if is_specific_event::<DisputeInitiated>(&event) {
		let decoded = decode_to_specific_event::<DisputeInitiated>(&event)?;
		ChainEvent::DisputeInitiated(SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 })
	} else if is_specific_event::<DisputeConcluded>(&event) {
		let decoded = decode_to_specific_event::<DisputeConcluded>(&event)?;
		let outcome = match decoded.1 {
			RuntimeDisputeResult::Valid => SubxtDisputeResult::Valid,
			RuntimeDisputeResult::Invalid => SubxtDisputeResult::Invalid,
		};
		ChainEvent::DisputeConcluded(
			SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 },
			outcome,
		)
	} else if is_specific_event::<CandidateBacked>(&event) {
		let decoded = decode_to_specific_event::<CandidateBacked>(&event)?;
		ChainEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.commitments_hash,
			decoded.0.descriptor,
			SubxtCandidateEventType::Backed,
		)))
	} else if is_specific_event::<CandidateIncluded>(&event) {
		let decoded = decode_to_specific_event::<CandidateIncluded>(&event)?;
		ChainEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.commitments_hash,
			decoded.0.descriptor,
			SubxtCandidateEventType::Included,
		)))
	} else if is_specific_event::<CandidateTimedOut>(&event) {
		let decoded = decode_to_specific_event::<CandidateTimedOut>(&event)?;
		ChainEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.commitments_hash,
			decoded.0.descriptor,
			SubxtCandidateEventType::TimedOut,
		)))
	} else {
		ChainEvent::RawEvent(block_hash, event.clone())
	};

	#[cfg(feature = "polkadot")]
	let subxt_event = if is_specific_event::<crate::metadata::polkadot::paras_disputes::events::DisputeTimedOut>(&event) {
		let decoded =
			decode_to_specific_event::<crate::metadata::polkadot::paras_disputes::events::DisputeTimedOut>(&event)?;
		ChainEvent::DisputeConcluded(
			SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0 .0 },
			SubxtDisputeResult::TimedOut,
		)
	} else {
		subxt_event
	};

	Ok(subxt_event)
}

fn is_specific_event<E: subxt::events::StaticEvent>(raw_event: &subxt::events::EventDetails) -> bool {
	E::is_event(raw_event.pallet_name(), raw_event.variant_name())
}

fn decode_to_specific_event<E: subxt::events::StaticEvent>(
	raw_event: &subxt::events::EventDetails,
) -> color_eyre::Result<E> {
	raw_event
		.as_event()
		.map_err(|e| {
			eyre!(
				"cannot decode event pallet {}, variant {}: {:?}",
				raw_event.pallet_name(),
				raw_event.variant_name(),
				e
			)
		})
		.and_then(|maybe_event| {
			maybe_event.ok_or_else(|| {
				eyre!(
					"cannot decode event pallet {}, variant {}: no event found",
					raw_event.pallet_name(),
					raw_event.variant_name(),
				)
			})
		})
}

fn create_candidate_event(
	commitments_hash: <PolkadotConfig as subxt::Config>::Hash,
	candidate_descriptor: CandidateDescriptor<<PolkadotConfig as subxt::Config>::Hash>,
	event_type: SubxtCandidateEventType,
) -> SubxtCandidateEvent {
	let candidate_hash = BlakeTwo256::hash_of(&(&candidate_descriptor, commitments_hash));
	let parachain_id = candidate_descriptor.para_id.0;
	SubxtCandidateEvent { event_type, candidate_descriptor, parachain_id, candidate_hash }
}
