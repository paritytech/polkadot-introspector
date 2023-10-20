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

use crate::metadata::{
	polkadot::{
		runtime_types as subxt_runtime_types,
		runtime_types::{polkadot_parachain::primitives::Id, polkadot_runtime_parachains::scheduler::AssignmentKind},
	},
	polkadot_primitives,
};
use parity_scale_codec::{Decode, Encode};
use std::collections::{BTreeMap, VecDeque};
use subxt::{
	config::substrate::{BlakeTwo256, SubstrateHeader},
	utils,
};
use subxt_runtime_types::polkadot_runtime as runtime;

pub type BlockNumber = u32;
pub type H256 = utils::H256;
pub type AccountId32 = utils::AccountId32;
pub type Header = SubstrateHeader<u32, BlakeTwo256>;
pub type Timestamp = u64;
pub type SessionKeys = runtime::SessionKeys;
pub type SubxtCall = runtime::RuntimeCall;
pub type ClaimQueue = BTreeMap<u32, VecDeque<Option<ParasEntry>>>;

/// The `InherentData` constructed with the subxt API.
pub type InherentData = polkadot_primitives::InherentData<
	subxt_runtime_types::sp_runtime::generic::header::Header<
		::core::primitive::u32,
		subxt_runtime_types::sp_runtime::traits::BlakeTwo256,
	>,
>;

/// A wrapper over subxt HRMP channel configuration
#[derive(Debug, Clone, Default)]
pub struct SubxtHrmpChannel {
	pub max_capacity: u32,
	pub max_total_size: u32,
	pub max_message_size: u32,
	pub msg_count: u32,
	pub total_size: u32,
	pub mqc_head: Option<H256>,
	pub sender_deposit: u128,
	pub recipient_deposit: u128,
}

impl From<subxt_runtime_types::polkadot_runtime_parachains::hrmp::HrmpChannel> for SubxtHrmpChannel {
	fn from(channel: subxt_runtime_types::polkadot_runtime_parachains::hrmp::HrmpChannel) -> Self {
		SubxtHrmpChannel {
			max_capacity: channel.max_capacity,
			max_total_size: channel.max_total_size,
			max_message_size: channel.max_message_size,
			msg_count: channel.msg_count,
			total_size: channel.total_size,
			mqc_head: channel.mqc_head,
			sender_deposit: channel.sender_deposit,
			recipient_deposit: channel.recipient_deposit,
		}
	}
}

// TODO: Take it from runtime types v5
/// How a free core is scheduled to be assigned.
pub struct CoreAssignment {
	/// The core that is assigned.
	pub core: polkadot_primitives::CoreIndex,
	/// The unique ID of the para that is assigned to the core.
	pub para_id: Id,
	/// The kind of the assignment.
	pub kind: AssignmentKind,
}

// TODO: Take it from runtime types v5
/// Polkadot v5 ParasEntry type
#[derive(Debug)]
pub struct ParasEntry {
	/// The `Assignment`
	pub assignment: Assignment,
	/// The number of times the entry has timed out in availability.
	pub availability_timeouts: u32,
	/// The block height where this entry becomes invalid.
	pub ttl: BlockNumber,
}

// TODO: Take it from runtime types v5
/// Polkadot v5 Assignment type
#[derive(Debug)]
pub struct Assignment {
	/// Assignment's ParaId
	pub para_id: u32,
}

// TODO: Take it from runtime types v5
/// Temporary abstraction to cover core state until v5 types are released
#[derive(Debug, Decode, Encode)]
pub enum CoreOccupied {
	/// The core is not occupied.
	Free,
	/// A paras.
	Paras,
}

// TODO: Take it from runtime types v5
/// Temporary abstraction to cover `Event::OnDemandAssignmentProvider`
#[derive(Debug, Decode, Encode, Default, Clone, PartialEq)]
pub struct OnDemandOrder {
	pub para_id: u32,
	pub spot_price: u128,
}
