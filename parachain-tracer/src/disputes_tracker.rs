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
use polkadot_introspector_essentials::{chain_events::SubxtDisputeResult, types::H256};

/// An outcome for a dispute
#[derive(Encode, Decode, Debug, Clone)]
pub struct DisputesTracker {
	/// Disputed candidate
	pub(crate) candidate: H256,
	/// The real outcome
	pub(crate) outcome: SubxtDisputeResult,
	/// Number of validators voted that a candidate is valid
	pub(crate) voted_for: u32,
	/// Number of validators voted that a candidate is invalid
	pub(crate) voted_against: u32,
	/// A vector of validators initiateds the dispute (index + identify)
	pub(crate) initiators: Vec<(u32, String)>,
	/// A vector of validators voted against supermajority (index + identify)
	pub(crate) misbehaving_validators: Vec<(u32, String)>,
	/// Dispute conclusion time: how many blocks have passed since DisputeInitiated event
	pub(crate) resolve_time: Option<u32>,
}
