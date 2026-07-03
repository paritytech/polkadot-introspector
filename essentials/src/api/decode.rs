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
	types::{H256, PolkadotHash},
};
use color_eyre::{Result, eyre::eyre};
use parity_scale_codec::{Decode, DecodeAll, Encode};
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
				RawCandidateEvent::TimedOut(receipt, _head, core) =>
					(receipt, SubxtCandidateEventType::TimedOut, core),
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
}
