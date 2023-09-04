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

use polkadot_introspector_essentials::types::H256;

/// Used to track forks of the relay chain
#[derive(Debug, Clone)]
pub struct ForkTracker {
	#[allow(dead_code)]
	pub(crate) relay_hash: H256,
	#[allow(dead_code)]
	pub(crate) relay_number: u32,
	pub(crate) backed_candidate: Option<H256>,
	pub(crate) included_candidate: Option<H256>,
}
