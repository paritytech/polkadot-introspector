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

use crate::metadata::polkadot::runtime_types as subxt_runtime_types;
use subxt::utils;

#[cfg(feature = "polkadot")]
use subxt_runtime_types::polkadot_runtime as runtime;
#[cfg(feature = "rococo")]
use subxt_runtime_types::rococo_runtime as runtime;

pub type BlockNumber = u32;
pub type H256 = utils::H256;
pub type AccountId32 = utils::AccountId32;
pub type Timestamp = u64;
pub type SessionKeys = runtime::SessionKeys;
pub type SubxtCall = runtime::RuntimeCall;
