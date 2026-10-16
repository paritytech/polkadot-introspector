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

use parity_scale_codec::{Compact, Decode, Encode, Error as CodecError, Input, Output};
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

/// Index of a validator in the session's validator set. Same SCALE layout as the runtime's `ValidatorIndex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct ValidatorIndex(pub u32);

/// Hash of a candidate receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct CandidateHash(pub H256);

/// Availability bits of one validator, one bit per core.
///
/// SCALE-encoded like `BitVec<u8, Lsb0>`: a compact bit count followed by `ceil(bits / 8)` bytes,
/// least-significant bit first within each byte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AvailabilityBitfield(pub Vec<bool>);

impl AvailabilityBitfield {
	pub fn get(&self, index: usize) -> Option<bool> {
		self.0.get(index).copied()
	}
}

impl Encode for AvailabilityBitfield {
	fn encode_to<T: Output + ?Sized>(&self, dest: &mut T) {
		Compact(self.0.len() as u32).encode_to(dest);
		for chunk in self.0.chunks(8) {
			let byte = chunk
				.iter()
				.enumerate()
				.fold(0u8, |acc, (bit, set)| acc | (u8::from(*set) << bit));
			dest.push_byte(byte);
		}
	}
}

impl Decode for AvailabilityBitfield {
	fn decode<I: Input>(input: &mut I) -> Result<Self, CodecError> {
		let bits = Compact::<u32>::decode(input)?.0 as usize;
		let mut out = Vec::with_capacity(bits.min(4096));
		for _ in 0..bits.div_ceil(8) {
			let byte = input.read_byte()?;
			for bit in 0..8 {
				if out.len() < bits {
					out.push(byte >> bit & 1 == 1);
				}
			}
		}
		Ok(Self(out))
	}
}

/// A dispute statement that supports the candidate.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
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
	ApprovalCheckingMultipleCandidates(Vec<CandidateHash>),
}

/// A dispute statement against the candidate.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum InvalidDisputeStatementKind {
	#[codec(index = 0)]
	Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum DisputeStatement {
	#[codec(index = 0)]
	Valid(ValidDisputeStatementKind),
	#[codec(index = 1)]
	Invalid(InvalidDisputeStatementKind),
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct InherentData {
	pub bitfields: Vec<AvailabilityBitfield>,
	pub disputes: Vec<DisputeStatementSet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct DisputeStatementSet {
	pub candidate_hash: CandidateHash,
	pub session: u32,
	pub statements: Vec<(DisputeStatement, ValidatorIndex)>,
}

/// HRMP channel configuration
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

/// Babe slot number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct Slot(pub u64);

/// Public key of a Babe authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BabeAuthorityId(pub [u8; 32]);

/// Public key a validator uses for authority discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityDiscoveryId(pub [u8; 32]);

/// The session keys the tools read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKeys {
	pub authority_discovery: AuthorityDiscoveryId,
}

/// Validators of the next session with their session keys.
pub type QueuedKeys = Vec<(AccountId32, SessionKeys)>;
