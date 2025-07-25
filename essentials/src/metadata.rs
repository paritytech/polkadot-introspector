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

#[subxt::subxt(
	runtime_metadata_path = "assets/polkadot_metadata.scale",
	derive_for_type(
		path = "polkadot_primitives::vstaging::CandidateDescriptorV2",
		derive = "parity_scale_codec::Decode, parity_scale_codec::Encode",
		recursive
	),
	derive_for_type(
		path = "polkadot_primitives::v8::ValidatorIndex",
		derive = "parity_scale_codec::Decode, parity_scale_codec::Encode",
		recursive
	),
	derive_for_type(
		path = "polkadot_primitives::v8::CoreIndex",
		derive = "parity_scale_codec::Decode, parity_scale_codec::Encode",
		recursive
	),
	derive_for_type(
		path = "polkadot_primitives::vstaging::InherentData",
		derive = "parity_scale_codec::Decode, parity_scale_codec::Encode",
		recursive
	),
	derive_for_type(
		path = "sp_consensus_babe::digests::PreDigest",
		derive = "parity_scale_codec::Decode, parity_scale_codec::Encode",
		recursive
	),
	derive_for_type(path = "sp_consensus_babe::app::Public", derive = "Clone", recursive)
)]
pub mod polkadot {}

pub use polkadot::runtime_types::polkadot_primitives::{
	v8 as polkadot_primitives, vstaging as polkadot_staging_primitives,
};
