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

use crate::{
	api::dynamic::decode_on_demand_order,
	metadata::{
		polkadot::{
			para_inclusion::events::{CandidateBacked, CandidateIncluded, CandidateTimedOut},
			paras_disputes::events::{DisputeConcluded, DisputeInitiated},
		},
		polkadot_staging_primitives::CandidateDescriptorV2,
	},
	types::{H256, Header, OnDemandOrder, PolkadotHash, PolkadotHasher},
};
use color_eyre::{Result, eyre::eyre};
use parity_scale_codec::{Decode, Encode};
use serde::Serialize;
use subxt::{PolkadotConfig, config::Hasher};

#[derive(Debug)]
pub enum ChainEvent<T: subxt::Config> {
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
	/// On-demand parachain placed its order
	OnDemandOrderPlaced(PolkadotHash, OnDemandOrder),
	/// Anything undecoded
	RawEvent(PolkadotHash, subxt::events::EventDetails<T>),
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
	pub candidate_hash: PolkadotHash,
	/// Full candidate receipt if needed
	pub candidate_descriptor: CandidateDescriptorV2<PolkadotHash>,
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

pub async fn decode_chain_event(
	block_hash: PolkadotHash,
	event: subxt::events::EventDetails<PolkadotConfig>,
	hasher: PolkadotHasher,
) -> Result<ChainEvent<PolkadotConfig>> {
	if is_specific_event::<DisputeInitiated, PolkadotConfig>(&event) {
		let decoded = decode_to_specific_event::<DisputeInitiated, PolkadotConfig>(&event)?;
		return Ok(ChainEvent::DisputeInitiated(SubxtDispute {
			relay_parent_block: block_hash,
			candidate_hash: decoded.0.0,
		}))
	}

	if is_specific_event::<DisputeConcluded, PolkadotConfig>(&event) {
		use crate::metadata::polkadot::runtime_types::polkadot_runtime_parachains::disputes;
		let decoded = decode_to_specific_event::<DisputeConcluded, PolkadotConfig>(&event)?;
		let outcome = match decoded.1 {
			disputes::DisputeResult::Valid => SubxtDisputeResult::Valid,
			disputes::DisputeResult::Invalid => SubxtDisputeResult::Invalid,
		};
		return Ok(ChainEvent::DisputeConcluded(
			SubxtDispute { relay_parent_block: block_hash, candidate_hash: decoded.0.0 },
			outcome,
		))
	}

	if is_specific_event::<CandidateBacked, PolkadotConfig>(&event) {
		let decoded = decode_to_specific_event::<CandidateBacked, PolkadotConfig>(&event)?;
		return Ok(ChainEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.commitments_hash,
			decoded.0.descriptor,
			decoded.2.0,
			SubxtCandidateEventType::Backed,
			hasher,
		))))
	}

	if is_specific_event::<CandidateIncluded, PolkadotConfig>(&event) {
		let decoded = decode_to_specific_event::<CandidateIncluded, PolkadotConfig>(&event)?;
		return Ok(ChainEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.commitments_hash,
			decoded.0.descriptor,
			decoded.2.0,
			SubxtCandidateEventType::Included,
			hasher,
		))))
	}

	if is_specific_event::<CandidateTimedOut, PolkadotConfig>(&event) {
		let decoded = decode_to_specific_event::<CandidateTimedOut, PolkadotConfig>(&event)?;
		return Ok(ChainEvent::CandidateChanged(Box::new(create_candidate_event(
			decoded.0.commitments_hash,
			decoded.0.descriptor,
			decoded.2.0,
			SubxtCandidateEventType::TimedOut,
			hasher,
		))))
	}

	// TODO: Use `is_specific_event` as soon as shows up in types
	if event.pallet_name() == "OnDemandAssignmentProvider" && event.variant_name() == "OnDemandOrderPlaced" {
		let decoded = decode_on_demand_order(&event.field_values()?)?;
		return Ok(ChainEvent::OnDemandOrderPlaced(block_hash, decoded))
	}

	Ok(ChainEvent::RawEvent(block_hash, event))
}

fn is_specific_event<E: subxt::events::StaticEvent, C: subxt::Config>(
	raw_event: &subxt::events::EventDetails<C>,
) -> bool {
	E::is_event(raw_event.pallet_name(), raw_event.variant_name())
}

fn decode_to_specific_event<E: subxt::events::StaticEvent, C: subxt::Config>(
	raw_event: &subxt::events::EventDetails<C>,
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
	commitments_hash: PolkadotHash,
	candidate_descriptor: CandidateDescriptorV2<PolkadotHash>,
	core_idx: u32,
	event_type: SubxtCandidateEventType,
	hasher: PolkadotHasher,
) -> SubxtCandidateEvent {
	let candidate_hash = hasher.hash_of(&(&candidate_descriptor, commitments_hash)).into();
	let parachain_id = candidate_descriptor.para_id.0;
	SubxtCandidateEvent { event_type, candidate_descriptor, parachain_id, candidate_hash, core_idx }
}
