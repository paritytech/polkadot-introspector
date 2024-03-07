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

use parity_scale_codec::{Decode, Encode};
use polkadot_introspector_essentials::{metadata::polkadot_primitives::BackedCandidate, types::H256};
use subxt::config::{substrate::BlakeTwo256, Hasher};

/// The parachain block tracking information.
/// This is used for displaying CLI updates and also goes to Storage.
#[derive(Encode, Decode, Debug, Default)]
pub struct ParachainBlockInfo {
	/// Candidate hash
	pub candidate_hash: Option<H256>,
	/// The number of signed bitfields.
	pub bitfield_count: u32,
	/// The maximum expected number of availability bits that can be set. Corresponds to `max_validators`.
	pub max_availability_bits: u32,
	/// The current number of observed availability bits set to 1.
	pub current_availability_bits: u32,
	/// Parachain availability core assignment information.
	pub assigned_core: Option<u32>,
	/// Core occupation status.
	pub core_occupied: bool,
	/// The current state.
	state: ParachainBlockState,
}

impl ParachainBlockInfo {
	pub fn new(candidate_hash: Option<H256>) -> Self {
		Self { candidate_hash, ..Default::default() }
	}

	pub fn candidate_hash(candidate: BackedCandidate<H256>) -> H256 {
		let commitments_hash = BlakeTwo256::hash_of(&candidate.candidate.commitments);
		BlakeTwo256::hash_of(&(&candidate.candidate.descriptor, commitments_hash))
	}

	pub fn set_idle(&mut self) {
		self.state = ParachainBlockState::Idle
	}

	pub fn set_backed(&mut self) {
		self.state = ParachainBlockState::Backed
	}

	pub fn set_pending(&mut self) {
		self.state = ParachainBlockState::PendingAvailability
	}

	pub fn set_included(&mut self) {
		self.state = ParachainBlockState::Included
	}

	pub fn is_idle(&self) -> bool {
		self.state == ParachainBlockState::Idle
	}

	pub fn is_backed(&self) -> bool {
		self.state == ParachainBlockState::Backed
	}

	pub fn is_pending(&self) -> bool {
		self.state == ParachainBlockState::PendingAvailability
	}

	pub fn is_included(&self) -> bool {
		self.state == ParachainBlockState::Included
	}

	pub fn is_bitfield_propagation_slow(&self) -> bool {
		self.max_availability_bits > 0 && !self.is_idle() && self.bitfield_count <= (self.max_availability_bits / 3) * 2
	}
}

/// The state of parachain block.
#[derive(Encode, Decode, Debug, Default, Clone, PartialEq, Eq)]
enum ParachainBlockState {
	// Parachain block pipeline is idle.
	#[default]
	Idle,
	// A candidate is currently backed.
	Backed,
	// A candidate is pending inclusion.
	PendingAvailability,
	// A candidate has been included.
	Included,
}

#[cfg(test)]
mod tests {
	use crate::test_utils::create_para_block_info;

	#[test]
	fn test_is_bitfield_propagation_slow() {
		let mut info = create_para_block_info();
		info.set_idle();
		assert!(!info.is_bitfield_propagation_slow());

		info.max_availability_bits = 200;
		assert!(!info.is_bitfield_propagation_slow());

		info.bitfield_count = 100;
		assert!(!info.is_bitfield_propagation_slow());

		info.set_backed();
		assert!(info.is_bitfield_propagation_slow());
	}
}
