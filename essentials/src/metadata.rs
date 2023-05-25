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

#[cfg(all(feature = "rococo", feature = "polkadot"))]
compile_error!("`rococo` and `polkadot` are mutually exclusive features");

#[cfg(not(any(feature = "rococo", feature = "polkadot")))]
compile_error!("Must build with either `rococo`, `polkadot` features");

#[cfg(feature = "rococo")]
#[subxt::subxt(runtime_metadata_path = "assets/rococo_metadata.scale")]
pub mod polkadot {}

#[cfg(feature = "rococo")]
pub use polkadot::runtime_types::polkadot_primitives::v4 as polkadot_primitives;

#[cfg(feature = "polkadot")]
#[subxt::subxt(runtime_metadata_path = "assets/polkadot_metadata.scale")]
pub mod polkadot {}

#[cfg(feature = "polkadot")]
pub use polkadot::runtime_types::polkadot_primitives::v2 as polkadot_primitives;
