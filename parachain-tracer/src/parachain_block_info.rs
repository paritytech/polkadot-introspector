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
use polkadot_introspector_essentials::{metadata::polkadot_staging_primitives::BackedCandidate, types::H256};
use subxt::{PolkadotConfig, config::Hasher};

/// The parachain block tracking information.
/// This is used for displaying CLI updates and also goes to Storage.
#[derive(Encode, Decode, Debug, Default)]
pub struct ParachainBlockInfo {
	/// Candidate hash
	pub candidate_hash: H256,
	/// The number of signed bitfields.
	pub bitfield_count: u32,
	/// The maximum expected number of availability bits that can be set. Corresponds to `max_validators`.
	pub max_availability_bits: u32,
	/// The current number of observed availability bits set to 1.
	pub current_availability_bits: u32,
	/// Parachain availability core assignment information.
	pub assigned_core: u32,
	/// Core occupation status.
	pub core_occupied: bool,
	/// The current state.
	pub state: ParachainBlockState,
}

impl ParachainBlockInfo {
	pub fn new(candidate_hash: H256, assigned_core: u32, bitfield_count: u32) -> Self {
		Self { candidate_hash, assigned_core, bitfield_count, ..Default::default() }
	}

	pub fn candidate_hash(
		candidate: &BackedCandidate<H256>,
		hasher: <PolkadotConfig as subxt::Config>::Hasher,
	) -> H256 {
		let commitments_hash = hasher.hash_of(&candidate.candidate.commitments);
		hasher.hash_of(&(&candidate.candidate.descriptor, commitments_hash))
	}

	pub fn set_pending(&mut self) {
		self.state = ParachainBlockState::PendingAvailability
	}

	pub fn set_included(&mut self) {
		self.state = ParachainBlockState::Included
	}

	pub fn set_dropped(&mut self) {
		self.state = ParachainBlockState::Dropped
	}

	pub fn is_backed(&self) -> bool {
		self.state == ParachainBlockState::Backed
	}

	pub fn is_included(&self) -> bool {
		self.state == ParachainBlockState::Included
	}

	pub fn is_dropped(&self) -> bool {
		self.state == ParachainBlockState::Dropped
	}

	pub fn is_bitfield_propagation_slow(&self) -> bool {
		self.max_availability_bits > 0 && self.bitfield_count <= (self.max_availability_bits / 3) * 2
	}
}

/// The state of parachain block.
#[derive(Encode, Decode, Debug, Default, Clone, PartialEq, Eq)]
pub enum ParachainBlockState {
	// A candidate is currently backed.
	#[default]
	Backed,
	// A candidate is pending inclusion.
	PendingAvailability,
	// A candidate has been included.
	Included,
	// A candidate has been dropped after session change or availability timeout
	Dropped,
}

#[cfg(test)]
mod tests {
	use crate::test_utils::{create_hasher, create_para_block_info};

	#[tokio::test]
	async fn test_is_bitfield_propagation_slow() {
		let hasher = create_hasher().await;
		let mut info = create_para_block_info(100, hasher);
		assert!(!info.is_bitfield_propagation_slow());

		info.max_availability_bits = 200;
		assert!(info.is_bitfield_propagation_slow());

		info.bitfield_count = 120;
		assert!(info.is_bitfield_propagation_slow());
	}
}
