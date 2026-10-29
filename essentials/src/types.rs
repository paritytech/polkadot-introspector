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

use bitvec::{order::Lsb0, vec::BitVec};
use parity_scale_codec::{Decode, Encode};
pub use primitive_types::{H256, U256};
use std::{collections::BTreeMap, fmt};

pub type BlockNumber = u32;
pub type PolkadotHash = H256;
pub type Timestamp = u64;
pub type ClaimQueue = Vec<(u32, Vec<u32>)>;
/// BABE slot. Must stay a `u64`: the author selection hashes its 8-byte SCALE encoding.
pub type Slot = u64;
/// Authority discovery key of every queued validator.
pub type QueuedKeys = Vec<(AccountId32, [u8; 32])>;
/// Payload of a signed bitfield in the `ParaInherent`: one bit per availability core, set when the
/// signing validator has the core's candidate available.
pub type AvailabilityBitfield = BitVec<u8, Lsb0>;

/// Blake2 256-bit hash of raw bytes: the hashing used for block hashes, candidate hashes and
/// storage-key derivation.
pub fn blake2_256_hash(data: &[u8]) -> H256 {
	H256::from(sp_core_hashing::blake2_256(data))
}

/// Blake2 256-bit hash of a SCALE-encoded value.
pub fn hash_of<T: Encode>(value: &T) -> H256 {
	blake2_256_hash(&value.encode())
}

/// Block hash of a header: Blake2 256-bit hash of its SCALE encoding.
pub fn header_hash(header: &Header) -> H256 {
	hash_of(header)
}

/// A 32-byte account ID. Displays as SS58 with the generic Substrate prefix (42).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode, Default)]
pub struct AccountId32(pub [u8; 32]);

impl From<[u8; 32]> for AccountId32 {
	fn from(bytes: [u8; 32]) -> Self {
		Self(bytes)
	}
}

/// SS58 checksum: the first two bytes of blake2b-512("SS58PRE" ++ data).
fn ss58_checksum(data: &[u8]) -> [u8; 2] {
	let hash = sp_core_hashing::blake2_512(&[b"SS58PRE", data].concat());
	[hash[0], hash[1]]
}

impl fmt::Display for AccountId32 {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let mut data = vec![42u8];
		data.extend_from_slice(&self.0);
		data.extend_from_slice(&ss58_checksum(&data));
		f.write_str(&bs58::encode(data).into_string())
	}
}

impl std::str::FromStr for AccountId32 {
	type Err = String;

	/// Parses an SS58 address with any single-byte prefix (Polkadot `0`, Kusama `2`, generic
	/// Substrate `42`, ...), verifying the checksum. Two-byte prefixes are rejected: no chain the
	/// tools talk to uses one.
	fn from_str(address: &str) -> Result<Self, Self::Err> {
		let raw = bs58::decode(address).into_vec().map_err(|e| format!("invalid base58: {e}"))?;
		if raw.len() != 35 || raw[0] >= 64 {
			return Err(format!("unsupported SS58 address: {address}"))
		}
		if ss58_checksum(&raw[..33]) != raw[33..] {
			return Err("invalid SS58 checksum".to_string())
		}
		Ok(Self(raw[1..33].try_into().expect("slice of 32 bytes")))
	}
}

/// A relay-chain block header, in SCALE order. The block hash is the hash of its encoding.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Header {
	pub parent_hash: H256,
	#[codec(compact)]
	pub number: BlockNumber,
	pub state_root: H256,
	pub extrinsics_root: H256,
	pub digest: Digest,
}

/// A block digest. The tools never read inside the items, but they are part of the hashed encoding.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Digest {
	pub logs: Vec<DigestItem>,
}

/// A digest item. The SCALE indices are the runtime's own, not declaration order: a wrong one
/// silently changes every computed block hash.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum DigestItem {
	#[codec(index = 6)]
	PreRuntime([u8; 4], Vec<u8>),
	#[codec(index = 4)]
	Consensus([u8; 4], Vec<u8>),
	#[codec(index = 5)]
	Seal([u8; 4], Vec<u8>),
	#[codec(index = 0)]
	Other(Vec<u8>),
	#[codec(index = 8)]
	RuntimeEnvironmentUpdated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct ValidatorIndex(pub u32);

/// The parts of `ParachainsInherentData` the tools read. It does not mirror the runtime layout:
/// bitfields keep only their payload (no validator index or signature), and backed candidates and
/// the parent header are dropped. Only `ValidatorIndex`, `DisputeStatement` and its kinds are
/// wire-compatible with the runtime.
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
/// reader must consume them to advance.
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

/// A wrapper over HRMP channel configuration
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

#[cfg(test)]
mod tests {
	use super::*;

	const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";

	#[test]
	fn account_id_ss58_roundtrip() {
		let account: AccountId32 = ALICE.parse().unwrap();
		assert_eq!(hex::encode(account.0), "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d");
		assert_eq!(account.to_string(), ALICE);
	}

	/// Polkadot mainnet block #20000000, fetched with `chain_getHeader`.
	#[test]
	fn header_hash_matches_polkadot_mainnet_block() {
		let h256 = |s: &str| H256::from_slice(&hex::decode(s).unwrap());
		let logs = [
			"0642414245b50103ee000000786cff1000000000b8ef2c01dc04245fae90b166429b432a194d668dd0cb0e26bfb0e045ccefe759002ff753aaafcb166cc8434e62318296794977952cff87ade1ad0f25b91fe606209f61de571534cb993f84287655f65e3ad8c620ab2bf2492eaea81dec748206",
			"044245454684032cf6add43c796218451a7d333028a34b9d6b18091640a0f5e1a298cb1b45fdb6",
			"05424142450101a47906d34a617e1293fe306c39fa524cbf014ecb7b93c6ec8624158b069e453f316329c57c4508f74322ac8c2f4366d475c2283db3b98726c45de2ed3eab3f81",
		];
		let header = Header {
			parent_hash: h256("5a1992595f211fd0d80d4dafffb766b39a8e1cbe139a7b6203dfd9abb20eae9b"),
			number: 0x1312d00,
			state_root: h256("9c76d70f391d1da9d8095007e92f63ee47ed06bf1279828fdadaeb12ecdaa202"),
			extrinsics_root: h256("f4ea1785f51aeaeedd0fc501de0c60cf7b9f8ec0ce25645ed4fb2dbc7b3b6819"),
			digest: Digest {
				logs: logs
					.iter()
					.map(|l| DigestItem::decode(&mut &hex::decode(l).unwrap()[..]).unwrap())
					.collect(),
			},
		};
		assert_eq!(header_hash(&header), h256("de2f6847c0cfed45b7a088fe0a726941a848a1337f185978483ef32a4efddfb4"));
	}
}
