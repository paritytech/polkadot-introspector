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

/// The parachain block tracking information.
/// This is used for displaying CLI updates and also goes to Storage.
#[derive(Encode, Decode, Debug, Default)]
pub struct ParachainBlockInfo {
	/// The candidate information as observed during backing
	pub(crate) candidate: Option<BackedCandidate<H256>>,
	/// Candidate hash
	pub(crate) candidate_hash: Option<H256>,
	/// The current state.
	state: ParachainBlockState,
	/// Backed on current block.
	just_backed: bool,
	/// The number of signed bitfields.
	pub(crate) bitfield_count: u32,
	/// The maximum expected number of availability bits that can be set. Corresponds to `max_validators`.
	pub(crate) max_availability_bits: u32,
	/// The current number of observed availability bits set to 1.
	pub(crate) current_availability_bits: u32,
	/// Parachain availability core assignment information.
	pub(crate) assigned_core: Option<u32>,
	/// Core occupation status.
	pub(crate) core_occupied: bool,
}

impl ParachainBlockInfo {
	pub(crate) fn maybe_reset(&mut self) {
		if self.is_included() {
			self.state = ParachainBlockState::Idle;
			self.candidate = None;
			self.candidate_hash = None;
		}
		self.just_backed = false;
	}

	pub(crate) fn set_idle(&mut self) {
		self.state = ParachainBlockState::Idle
	}

	pub(crate) fn set_backed(&mut self) {
		self.state = ParachainBlockState::Backed
	}

	pub(crate) fn set_just_backed(&mut self) {
		self.just_backed = true;
		self.set_backed();
	}

	pub(crate) fn set_pending(&mut self) {
		self.state = ParachainBlockState::PendingAvailability
	}

	pub(crate) fn set_included(&mut self) {
		self.state = ParachainBlockState::Included
	}

	pub(crate) fn is_idle(&self) -> bool {
		self.state == ParachainBlockState::Idle
	}

	pub(crate) fn is_backed(&self) -> bool {
		self.state == ParachainBlockState::Backed
	}

	pub(super) fn is_just_backed(&self) -> bool {
		self.just_backed
	}

	pub(crate) fn is_pending(&self) -> bool {
		self.state == ParachainBlockState::PendingAvailability
	}

	pub(crate) fn is_included(&self) -> bool {
		self.state == ParachainBlockState::Included
	}

	pub(crate) fn is_data_available(&self) -> bool {
		self.current_availability_bits > (self.max_availability_bits / 3) * 2
	}

	pub(crate) fn is_bitfield_propagation_low(&self) -> bool {
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
