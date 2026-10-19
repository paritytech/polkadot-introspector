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

use crate::metadata::polkadot::runtime_types::polkadot_runtime_parachains::hrmp::HrmpChannel;
use bitvec::{order::Lsb0, vec::BitVec};
use parity_scale_codec::{Decode, Encode};
use std::collections::BTreeMap;
use subxt::{
	config::substrate::{DynamicHasher256, SubstrateHeader},
	utils,
};

pub type BlockNumber = u32;
pub type H256 = utils::H256;
pub type AccountId32 = utils::AccountId32;
pub type Header = SubstrateHeader<u32, DynamicHasher256>;
pub type PolkadotHash = subxt::config::HashFor<subxt::PolkadotConfig>;
pub type PolkadotHasher = <subxt::PolkadotConfig as subxt::Config>::Hasher;
pub type Timestamp = u64;
pub type ClaimQueue = Vec<(u32, Vec<u32>)>;
/// BABE slot. Must stay a `u64`: the author selection hashes its 8-byte SCALE encoding.
pub type Slot = u64;
/// Authority discovery key of every queued validator
pub type QueuedKeys = Vec<(AccountId32, [u8; 32])>;
/// One bit per core, set when the validator has the core's candidate available
pub type AvailabilityBitfield = BitVec<u8, Lsb0>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct ValidatorIndex(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct InherentData {
	pub bitfields: Vec<AvailabilityBitfield>,
	pub disputes: Vec<DisputeStatementSet>,
}

/// A `DisputeStatementSet` from the inherent: the candidate and session under dispute and each
/// statement with its validator. Signatures are dropped.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct DisputeStatementSet {
	pub candidate_hash: H256,
	pub session: u32,
	pub statements: Vec<(DisputeStatement, ValidatorIndex)>,
}

/// `DisputeStatement`.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
pub enum DisputeStatement {
	#[codec(index = 0)]
	Valid(ValidDisputeStatementKind),
	#[codec(index = 1)]
	Invalid(InvalidDisputeStatementKind),
}

/// `ValidDisputeStatementKind`: variant indices mirror the runtime. The hashes are kept because a
/// reader must consume them to advance, and they make comparison to metadata decoding precise.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
pub enum ValidDisputeStatementKind {
	#[codec(index = 0)]
	Explicit,
	#[codec(index = 1)]
	BackingSeconded(H256),
	#[codec(index = 2)]
	BackingValid(H256),
	#[codec(index = 3)]
	ApprovalChecking,
	#[codec(index = 4)]
	ApprovalCheckingMultipleCandidates(Vec<H256>),
}

/// `InvalidDisputeStatementKind`.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
pub enum InvalidDisputeStatementKind {
	#[codec(index = 0)]
	Explicit,
}

/// A wrapper over subxt HRMP channel configuration
#[derive(Debug, Clone, Default, Encode, Decode, PartialEq, Eq)]
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

impl From<&HrmpChannel> for SubxtHrmpChannel {
	fn from(channel: &HrmpChannel) -> Self {
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

impl From<HrmpChannel> for SubxtHrmpChannel {
	fn from(channel: HrmpChannel) -> Self {
		Self::from(&channel)
	}
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

/// Abstraction to cover core states
#[derive(Debug, Decode, Encode, PartialEq, Eq)]
pub enum CoreOccupied {
	/// The core is not occupied.
	Free,
	/// The core is currently free, with a para scheduled
	Scheduled,
	/// The core is occupied by a para
	Occupied,
}

pub type InboundOutBoundHrmpChannels = Vec<(u32, BTreeMap<u32, SubxtHrmpChannel>, BTreeMap<u32, SubxtHrmpChannel>)>;
