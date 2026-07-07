// Copyright 2024 Parity Technologies (UK) Ltd.
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

//! Metadata-free decoders for the chain data the tools read.
//!
//! Each decoder reads the leading fields we use, positionally, and ignores the rest. This tolerates
//! fields appended in a runtime upgrade but fails loud on a field inserted-before or retyped, so a
//! layout change never yields a silently wrong value. Decoders are free functions kept independent
//! of the transport, so a later transport refactor deletes call sites rather than rewriting logic.

use crate::{
	chain_events::{SubxtCandidateEvent, SubxtCandidateEventType},
	types::{CoreOccupied, H256, PolkadotHash},
};
use color_eyre::{Result, eyre::eyre};
use parity_scale_codec::{Compact, Decode, DecodeAll, Encode, Error as CodecError, Input};
use subxt::config::Hasher;

/// SCALE-encoded size of a `CandidateReceipt`: `CandidateDescriptor` (292 bytes) plus the
/// `commitments_hash` (32 bytes). The size is stable across descriptor versions (V1's fields are
/// re-interpreted, never resized), so decoding this fixed window handles every version alike.
const CANDIDATE_RECEIPT_SIZE: usize = 324;
/// Bytes of the receipt after `para_id` (`u32`) and `relay_parent` (`H256`), kept opaque.
const RECEIPT_TAIL_SIZE: usize = CANDIDATE_RECEIPT_SIZE - 4 - 32;

/// A `CandidateReceipt` decoded only as far as the fields we use. `para_id` and `relay_parent` lead
/// every descriptor version; the remaining bytes are opaque and re-encoded solely to recompute the
/// candidate hash. Fixing the tail length makes a resized receipt fail to decode rather than pass.
#[derive(Encode, Decode)]
struct RawCandidateReceipt {
	para_id: u32,
	relay_parent: [u8; 32],
	tail: [u8; RECEIPT_TAIL_SIZE],
}

/// One element of the `ParachainHost_candidate_events` result:
/// `(CandidateReceipt, HeadData, CoreIndex[, GroupIndex])`, keyed by the same variant indices the
/// runtime uses. `HeadData` (`Vec<u8>`), `CoreIndex` and `GroupIndex` (`u32`) are decoded through
/// their SCALE shapes so codec advances past them; only `CoreIndex` is kept.
#[derive(Decode)]
enum RawCandidateEvent {
	#[codec(index = 0)]
	Backed(RawCandidateReceipt, Vec<u8>, u32, u32),
	#[codec(index = 1)]
	Included(RawCandidateReceipt, Vec<u8>, u32, u32),
	#[codec(index = 2)]
	TimedOut(RawCandidateReceipt, Vec<u8>, u32),
}

/// Decodes the SCALE-encoded result of the `ParachainHost_candidate_events` runtime call into the
/// candidate events the tools track. Codec drives the traversal (vec length, variant tags, the
/// variable-length `HeadData`); `decode_all` fails loud on any trailing bytes or unknown variant,
/// and the candidate hash is `blake2_256` of the re-encoded receipt, as the runtime computes it.
pub fn decode_candidate_events<H: Hasher<Output = PolkadotHash>>(
	bytes: &[u8],
	hasher: H,
) -> Result<Vec<SubxtCandidateEvent>> {
	let raw = Vec::<RawCandidateEvent>::decode_all(&mut &bytes[..])
		.map_err(|e| eyre!("candidate_events: cannot decode: {e}"))?;

	Ok(raw
		.into_iter()
		.map(|event| {
			let (receipt, event_type, core_idx) = match event {
				RawCandidateEvent::Backed(receipt, _head, core, _group) =>
					(receipt, SubxtCandidateEventType::Backed, core),
				RawCandidateEvent::Included(receipt, _head, core, _group) =>
					(receipt, SubxtCandidateEventType::Included, core),
				RawCandidateEvent::TimedOut(receipt, _head, core) => (receipt, SubxtCandidateEventType::TimedOut, core),
			};
			SubxtCandidateEvent {
				candidate_hash: hasher.hash(&receipt.encode()),
				relay_parent: H256::from(receipt.relay_parent),
				parachain_id: receipt.para_id,
				event_type,
				core_idx,
			}
		})
		.collect())
}

/// One recent dispute from the `ParachainHost_disputes` runtime call, decoded only as far as the
/// fields the tools use: `(SessionIndex, CandidateHash, DisputeState)`. The two validator bitsets are
/// kept as the indices of their set bits — enough to tally each side and derive the outcome — and
/// `start` / `concluded_at` give the relay block the dispute was recorded and (if any) concluded at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedDispute {
	pub session: u32,
	pub candidate_hash: PolkadotHash,
	pub validators_for: Vec<u32>,
	pub validators_against: Vec<u32>,
	pub start: u32,
	pub concluded_at: Option<u32>,
}

/// Decodes the SCALE-encoded result of the `ParachainHost_disputes` runtime call. The layout of each
/// element is `SessionIndex ++ CandidateHash ++ DisputeState`, and `DisputeState` is
/// `validators_for ++ validators_against ++ start ++ concluded_at` — the two bitsets come first, so
/// their variable length is consumed before the fixed `start` / `concluded_at` fields. Fails loud on
/// truncation or trailing bytes, so a layout change never yields a silently wrong value.
pub fn decode_disputes(bytes: &[u8]) -> Result<Vec<DecodedDispute>> {
	let mut input = bytes;
	let len = Compact::<u32>::decode(&mut input)
		.map_err(|e| eyre!("disputes: cannot decode length prefix: {e}"))?
		.0;

	let mut disputes = Vec::with_capacity(len as usize);
	for i in 0..len {
		let session =
			u32::decode(&mut input).map_err(|e| eyre!("disputes: cannot decode session for dispute {i}: {e}"))?;
		let candidate_hash = <[u8; 32]>::decode(&mut input)
			.map_err(|e| eyre!("disputes: cannot decode candidate_hash for dispute {i}: {e}"))?;
		let validators_for = decode_bitset_indices(&mut input)
			.map_err(|e| eyre!("disputes: cannot decode validators_for for dispute {i}: {e}"))?;
		let validators_against = decode_bitset_indices(&mut input)
			.map_err(|e| eyre!("disputes: cannot decode validators_against for dispute {i}: {e}"))?;
		let start = u32::decode(&mut input).map_err(|e| eyre!("disputes: cannot decode start for dispute {i}: {e}"))?;
		let concluded_at = Option::<u32>::decode(&mut input)
			.map_err(|e| eyre!("disputes: cannot decode concluded_at for dispute {i}: {e}"))?;

		disputes.push(DecodedDispute {
			session,
			candidate_hash: H256::from(candidate_hash),
			validators_for,
			validators_against,
			start,
			concluded_at,
		});
	}

	if !input.is_empty() {
		return Err(eyre!("disputes: {} trailing bytes after {len} disputes", input.len()));
	}

	Ok(disputes)
}

/// Decodes a SCALE-encoded `BitVec<u8, Lsb0>` into the indices of its set bits. The encoding is a
/// compact bit count followed by `ceil(bits / 8)` bytes, least-significant bit first within each
/// byte. Errors rather than guesses if the byte payload is shorter than the declared bit count.
fn decode_bitset_indices(input: &mut &[u8]) -> Result<Vec<u32>> {
	let bit_len = Compact::<u32>::decode(input)
		.map_err(|e| eyre!("cannot decode bitset length: {e}"))?
		.0 as usize;
	let byte_len = bit_len.div_ceil(8);
	if input.len() < byte_len {
		return Err(eyre!("bitset payload too short: need {byte_len} bytes, have {}", input.len()));
	}
	let (bytes, rest) = input.split_at(byte_len);
	*input = rest;

	Ok((0..bit_len)
		.filter(|&i| bytes[i / 8] & (1 << (i % 8)) != 0)
		.map(|i| i as u32)
		.collect())
}

/// Consumes a SCALE-encoded `BitVec<u8, Lsb0>` (compact bit length followed by `ceil(bits / 8)`
/// packed bytes) without retaining it, so the decoder advances past `OccupiedCore::availability` —
/// a field the tools don't read. parity-scale-codec offers no derive for bitvecs, hence this manual
/// `Decode` used as a field type below.
struct SkipBitVec;

impl Decode for SkipBitVec {
	fn decode<I: Input>(input: &mut I) -> core::result::Result<Self, CodecError> {
		let bit_len = Compact::<u32>::decode(input)?.0 as usize;
		let mut buf = vec![0u8; bit_len.div_ceil(8)];
		input.read(&mut buf)?;
		Ok(SkipBitVec)
	}
}

/// `polkadot_primitives::ScheduledCore`. Only decoded to advance the cursor; fields go unread.
#[derive(Decode)]
#[allow(dead_code)]
struct RawScheduledCore {
	para_id: u32,
	collator: Option<[u8; 32]>,
}

/// `polkadot_primitives::OccupiedCore`, decoded in full only to advance the cursor past an occupied
/// core — the tools keep just the `CoreState` variant. `candidate_descriptor` is the fixed 292-byte
/// `CandidateDescriptorV2` window (see [`CANDIDATE_RECEIPT_SIZE`]); pinning its length makes a
/// resized descriptor fail to decode rather than silently desync the surrounding vector.
#[derive(Decode)]
#[allow(dead_code)]
struct RawOccupiedCore {
	next_up_on_available: Option<RawScheduledCore>,
	occupied_since: u32,
	time_out_at: u32,
	next_up_on_time_out: Option<RawScheduledCore>,
	availability: SkipBitVec,
	group_responsible: u32,
	candidate_hash: [u8; 32],
	candidate_descriptor: [u8; CANDIDATE_RECEIPT_SIZE - 32],
}

/// One element of the `ParachainHost_availability_cores` result, keyed by the runtime's own variant
/// indices. Payloads are decoded through their SCALE shapes so the cursor advances; only the variant
/// is kept.
#[derive(Decode)]
#[allow(dead_code)] // payloads are decoded to advance the cursor, then discarded; only the tag is read
enum RawCoreState {
	#[codec(index = 0)]
	Occupied(Box<RawOccupiedCore>),
	#[codec(index = 1)]
	Scheduled(RawScheduledCore),
	#[codec(index = 2)]
	Free,
}

/// Decodes the SCALE-encoded result of the `ParachainHost_availability_cores` runtime call into the
/// per-core states the tools track, preserving order (cores are addressed by index). Codec drives
/// the traversal and `decode_all` fails loud on trailing bytes or an unknown variant.
pub fn decode_availability_cores(bytes: &[u8]) -> Result<Vec<CoreOccupied>> {
	let raw = Vec::<RawCoreState>::decode_all(&mut &bytes[..])
		.map_err(|e| eyre!("availability_cores: cannot decode: {e}"))?;

	Ok(raw
		.into_iter()
		.map(|core| match core {
			RawCoreState::Occupied(_) => CoreOccupied::Occupied,
			RawCoreState::Scheduled(_) => CoreOccupied::Scheduled,
			RawCoreState::Free => CoreOccupied::Free,
		})
		.collect())
}

#[cfg(test)]
mod tests {
	use super::*;
	use parity_scale_codec::Compact;
	use subxt::config::substrate::BlakeTwo256;

	// Builds one SCALE-encoded `CandidateEvent` with the given variant, para id, relay parent and
	// core index, mirroring `(CandidateReceipt, HeadData, CoreIndex[, GroupIndex])`.
	fn encode_event(variant: u8, para_id: u32, relay_parent: [u8; 32], core_idx: u32) -> (Vec<u8>, [u8; 324]) {
		let mut receipt = [0u8; CANDIDATE_RECEIPT_SIZE];
		receipt[..4].copy_from_slice(&para_id.to_le_bytes());
		receipt[4..36].copy_from_slice(&relay_parent);

		let mut bytes = vec![variant];
		bytes.extend_from_slice(&receipt);
		bytes.extend(vec![1u8, 2, 3].encode()); // HeadData
		bytes.extend(core_idx.encode());
		if variant != 2 {
			bytes.extend(42u32.encode()); // GroupIndex, absent for TimedOut
		}
		(bytes, receipt)
	}

	#[test]
	fn decodes_leading_fields_and_hashes_the_receipt_window() {
		let (backed, receipt0) = encode_event(0, 1000, [0xAB; 32], 5);
		let (timed_out, receipt1) = encode_event(2, 2000, [0xCD; 32], 9);

		let mut blob = Compact(2u32).encode();
		blob.extend(backed);
		blob.extend(timed_out);

		let events = decode_candidate_events(&blob, BlakeTwo256).unwrap();
		assert_eq!(events.len(), 2);

		assert_eq!(events[0].parachain_id, 1000);
		assert_eq!(events[0].relay_parent, H256::from([0xAB; 32]));
		assert_eq!(events[0].core_idx, 5);
		assert_eq!(events[0].event_type, SubxtCandidateEventType::Backed);
		assert_eq!(events[0].candidate_hash, BlakeTwo256.hash(&receipt0));

		assert_eq!(events[1].parachain_id, 2000);
		assert_eq!(events[1].core_idx, 9);
		assert_eq!(events[1].event_type, SubxtCandidateEventType::TimedOut);
		assert_eq!(events[1].candidate_hash, BlakeTwo256.hash(&receipt1));
	}

	#[test]
	fn rejects_trailing_bytes() {
		let (backed, _) = encode_event(0, 1, [0; 32], 0);
		let mut blob = Compact(1u32).encode();
		blob.extend(backed);
		blob.push(0xFF); // one byte too many
		assert!(decode_candidate_events(&blob, BlakeTwo256).is_err());
	}

	#[test]
	fn rejects_truncated_receipt() {
		let mut blob = Compact(1u32).encode();
		blob.push(0); // variant, then nothing
		assert!(decode_candidate_events(&blob, BlakeTwo256).is_err());
	}

	#[test]
	fn rejects_unknown_variant() {
		let (mut bogus, _) = encode_event(0, 1, [0; 32], 0);
		bogus[0] = 7; // unknown CandidateEvent variant
		let mut blob = Compact(1u32).encode();
		blob.extend(bogus);
		assert!(decode_candidate_events(&blob, BlakeTwo256).is_err());
	}

	// SCALE-encodes a `BitVec<u8, Lsb0>` over the given set-bit indices: compact bit count then the
	// packed bytes, least-significant bit first.
	fn encode_bitset(set_indices: &[u32], bit_len: usize) -> Vec<u8> {
		let byte_len = bit_len.div_ceil(8);
		let mut bytes = vec![0u8; byte_len];
		for &i in set_indices {
			bytes[i as usize / 8] |= 1 << (i as usize % 8);
		}
		let mut out = Compact(bit_len as u32).encode();
		out.extend(bytes);
		out
	}

	// Encodes one `(SessionIndex, CandidateHash, DisputeState)` element.
	fn encode_dispute(
		session: u32,
		candidate_hash: [u8; 32],
		validators_for: &[u32],
		validators_against: &[u32],
		bit_len: usize,
		start: u32,
		concluded_at: Option<u32>,
	) -> Vec<u8> {
		let mut out = session.encode();
		out.extend(candidate_hash);
		out.extend(encode_bitset(validators_for, bit_len));
		out.extend(encode_bitset(validators_against, bit_len));
		out.extend(start.encode());
		out.extend(concluded_at.encode());
		out
	}

	#[test]
	fn decodes_disputes_positionally() {
		let ongoing = encode_dispute(100, [0xAA; 32], &[0, 2, 5], &[1], 8, 42, None);
		let concluded = encode_dispute(101, [0xBB; 32], &[3], &[0, 1, 2, 4], 8, 40, Some(50));

		let mut blob = Compact(2u32).encode();
		blob.extend(ongoing);
		blob.extend(concluded);

		let disputes = decode_disputes(&blob).unwrap();
		assert_eq!(disputes.len(), 2);

		assert_eq!(disputes[0].session, 100);
		assert_eq!(disputes[0].candidate_hash, H256::from([0xAA; 32]));
		assert_eq!(disputes[0].validators_for, vec![0, 2, 5]);
		assert_eq!(disputes[0].validators_against, vec![1]);
		assert_eq!(disputes[0].start, 42);
		assert_eq!(disputes[0].concluded_at, None);

		assert_eq!(disputes[1].session, 101);
		assert_eq!(disputes[1].candidate_hash, H256::from([0xBB; 32]));
		assert_eq!(disputes[1].validators_against, vec![0, 1, 2, 4]);
		assert_eq!(disputes[1].concluded_at, Some(50));
	}

	#[test]
	fn decodes_empty_disputes() {
		let blob = Compact(0u32).encode();
		assert!(decode_disputes(&blob).unwrap().is_empty());
	}

	#[test]
	fn rejects_disputes_trailing_bytes() {
		let mut blob = Compact(1u32).encode();
		blob.extend(encode_dispute(1, [0; 32], &[0], &[], 8, 1, None));
		blob.push(0xFF); // one byte too many
		assert!(decode_disputes(&blob).is_err());
	}

	#[test]
	fn rejects_disputes_truncated_bitset() {
		let mut blob = Compact(1u32).encode();
		blob.extend(1u32.encode()); // session
		blob.extend([0u8; 32]); // candidate_hash
		blob.extend(Compact(64u32).encode()); // claims 64 bits, but no payload bytes follow
		assert!(decode_disputes(&blob).is_err());
	}

	// Encodes a `ScheduledCore { para_id, collator: None }`.
	fn encode_scheduled_core(para_id: u32) -> Vec<u8> {
		let mut bytes = para_id.encode();
		bytes.push(0x00); // collator: None
		bytes
	}

	// Encodes an `OccupiedCore` with the given availability bit count, exercising `SkipBitVec`.
	fn encode_occupied_core(availability_bits: u32) -> Vec<u8> {
		let mut bytes = vec![0x00]; // next_up_on_available: None
		bytes.extend(10u32.encode()); // occupied_since
		bytes.extend(20u32.encode()); // time_out_at
		bytes.push(0x00); // next_up_on_time_out: None
		bytes.extend(Compact(availability_bits).encode());
		bytes.extend(vec![0xFFu8; (availability_bits as usize).div_ceil(8)]); // availability payload
		bytes.extend(7u32.encode()); // group_responsible
		bytes.extend([0xAAu8; 32]); // candidate_hash
		bytes.extend([0xBBu8; CANDIDATE_RECEIPT_SIZE - 32]); // candidate_descriptor
		bytes
	}

	#[test]
	fn decodes_core_states_in_order() {
		let mut blob = Compact(4u32).encode();
		blob.push(2); // Free
		blob.push(1);
		blob.extend(encode_scheduled_core(1000)); // Scheduled
		blob.push(0);
		blob.extend(encode_occupied_core(0)); // Occupied, empty availability
		blob.push(0);
		blob.extend(encode_occupied_core(20)); // Occupied, 20-bit availability (3 bytes)

		let cores = decode_availability_cores(&blob).unwrap();
		assert_eq!(
			cores,
			vec![CoreOccupied::Free, CoreOccupied::Scheduled, CoreOccupied::Occupied, CoreOccupied::Occupied]
		);
	}

	#[test]
	fn rejects_core_states_trailing_bytes() {
		let mut blob = Compact(1u32).encode();
		blob.push(2); // Free
		blob.push(0xFF); // one byte too many
		assert!(decode_availability_cores(&blob).is_err());
	}

	#[test]
	fn rejects_core_states_unknown_variant() {
		let mut blob = Compact(1u32).encode();
		blob.push(3); // unknown CoreState variant
		assert!(decode_availability_cores(&blob).is_err());
	}

	#[test]
	fn rejects_core_states_truncated_occupied_core() {
		let mut blob = Compact(1u32).encode();
		blob.push(0); // Occupied variant, then a truncated payload
		blob.push(0x00); // next_up_on_available: None, nothing after
		assert!(decode_availability_cores(&blob).is_err());
	}
}
