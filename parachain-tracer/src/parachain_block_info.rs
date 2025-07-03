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
use polkadot_introspector_essentials::types::H256;

#[derive(Encode, Decode, Debug, Default)]
pub struct BlockAvailability {
	/// The current number of observed availability bits set to 1.
	pub current: u32,
	/// The maximum expected number of availability bits that can be set. Corresponds to `max_validators`.
	pub max: u32,
}

impl BlockAvailability {
	pub fn new(current: u32, max: u32) -> Self {
		Self { current, max }
	}

	pub fn is_slow(&self) -> bool {
		self.max > 0 && self.current <= (self.max / 3) * 2
	}
}

/// The parachain block tracking information.
/// This is used for displaying CLI updates and also goes to Storage.
#[derive(Encode, Decode, Debug, Default)]
pub struct ParachainBlockInfo {
	/// Candidate hash
	pub candidate_hash: H256,
	/// The availability for the block.
	pub availability: Option<BlockAvailability>,
	/// Parachain availability core assignment information.
	pub assigned_core: u32,
	/// Core occupation status.
	pub core_occupied: bool,
	/// The current state.
	pub state: ParachainBlockState,
}

impl ParachainBlockInfo {
	pub fn new(candidate_hash: H256, assigned_core: u32) -> Self {
		Self { candidate_hash, assigned_core, ..Default::default() }
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

	pub fn set_availability(&mut self, availability: BlockAvailability) {
		self.availability = Some(availability);
	}

	pub fn is_bitfield_propagation_slow(&self) -> bool {
		self.availability.as_ref().map_or(false, |availability| availability.is_slow())
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
	use crate::{
		parachain_block_info::BlockAvailability,
		test_utils::{create_hasher, create_para_block_info},
	};

	#[tokio::test]
	async fn test_is_bitfield_propagation_slow() {
		let hasher = create_hasher().await;
		let mut info = create_para_block_info(100, hasher);
		assert!(!info.is_bitfield_propagation_slow());

		info.set_availability(BlockAvailability::new(120, 200));
		assert!(info.is_bitfield_propagation_slow());
	}
}
