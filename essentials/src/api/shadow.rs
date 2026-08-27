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

//! Shadow-decode harness used to migrate reads off the chain metadata.
//!
//! While a read is being migrated it is decoded twice at the same block: once through the metadata
//! (the value the tool returns) and once through the new metadata-free path. The two are compared
//! here. A mismatch is a decoder defect, so it aborts the process loudly rather than being tallied
//! and passed over. The harness is temporary: it is gone once metadata is deleted.
//!
//! A checklist tallies clean comparisons per read, distinguishing "ran clean" from "never ran": a
//! quiet run proves only the reads it exercised, and rare paths (a real dispute, a skipped slot)
//! stay `pending` until a run actually hits them. It is logged periodically as a logfmt line
//! (graphable from collected logs) and, via [`log_checklist`], at shutdown.

use crate::types::H256;
use log::{error, info, warn};
use std::{
	collections::BTreeMap,
	fmt::Debug,
	sync::{Mutex, OnceLock},
	time::{Duration, Instant},
};

/// Every shadowed read, in display order. The checklist is seeded with the full list so a read that
/// never fired shows as `pending` instead of being silently absent — a quiet run only proves the
/// reads it actually exercised.
const EXPECTED_READS: &[&str] = &[
	"block_header",
	"block_number",
	"block_timestamp",
	"candidate_events",
	"disputes_initiated",
	"disputes_concluded",
	"availability_cores",
	"claim_queue",
	"validator_groups",
	"session_index",
	"session_account_keys",
	"session_queued_keys",
	"session_key_owner",
	"babe_randomness",
	"babe_authorities",
	"babe_current_slot",
	"hrmp_egress_channels_index",
	"hrmp_channel_digests",
	"hrmp_channel",
	"parainherent_bitfields",
];

/// How often the checklist is logged while comparisons keep coming in. Short enough to give a
/// usable resolution when the logfmt lines are graphed from collected logs.
const LOG_INTERVAL: Duration = Duration::from_secs(60);

struct Checklist {
	/// Clean-comparison count per read, indexed like [`EXPECTED_READS`].
	counts: [u64; EXPECTED_READS.len()],
	last_logged: Instant,
}

fn checklist() -> &'static Mutex<Checklist> {
	static CHECKLIST: OnceLock<Mutex<Checklist>> = OnceLock::new();
	CHECKLIST.get_or_init(|| Mutex::new(Checklist { counts: [0; EXPECTED_READS.len()], last_logged: Instant::now() }))
}

/// Records a clean comparison for `read` and logs the checklist at most every [`LOG_INTERVAL`].
fn mark(read: &str) {
	let mut list = checklist()
		.lock()
		.expect("no panic can occur while the checklist is locked; qed");
	match EXPECTED_READS.iter().position(|&expected| expected == read) {
		Some(idx) => list.counts[idx] += 1,
		None => warn!("shadow read `{read}` is missing from the checklist's EXPECTED_READS"),
	}
	if list.last_logged.elapsed() >= LOG_INTERVAL {
		list.last_logged = Instant::now();
		info!("{}", render_logfmt(&list.counts));
	}
}

/// Renders the checklist as one logfmt line (`key=value` pairs) so a log collector (e.g.
/// Loki/promtail into Grafana) can parse it into per-read time series without custom rules.
fn render_logfmt(counts: &[u64]) -> String {
	let exercised = counts.iter().filter(|&&count| count > 0).count();
	let mut out = format!("shadow_checklist exercised={exercised} total={}", counts.len());
	for (read, count) in EXPECTED_READS.iter().zip(counts) {
		out.push_str(&format!(" {read}={count}"));
	}
	out
}

fn render_checklist(counts: &[u64]) -> String {
	let exercised = counts.iter().filter(|&&count| count > 0).count();
	let mut out = format!("shadow-decode checklist: {exercised}/{} reads exercised", counts.len());
	for (read, count) in EXPECTED_READS.iter().zip(counts) {
		if *count > 0 {
			out.push_str(&format!("\n  ok      {read} ({count})"));
		} else {
			out.push_str(&format!("\n  pending {read}"));
		}
	}
	out
}

/// Logs the checklist unconditionally, both as a human-readable table and as a final logfmt line.
/// Call at shutdown so a shadow run ends with a coverage verdict: which reads it proved (with
/// clean-comparison counts) and which are still pending.
pub fn log_checklist() {
	let list = checklist()
		.lock()
		.expect("no panic can occur while the checklist is locked; qed");
	info!("{}", render_logfmt(&list.counts));
	info!("{}", render_checklist(&list.counts));
}

/// Compares the metadata decode (`old`) against the metadata-free decode (`new`) for a single
/// scalar read at `block`. Aborts the process on any mismatch, reporting `(block, read, old, new)`.
pub fn compare<T: PartialEq + Debug>(block: H256, read: &str, old: &T, new: &T) {
	if old != new {
		report_and_abort(block, read, &format!("{old:?}"), &format!("{new:?}"));
	}
	mark(read);
}

/// Compares two collections as sets keyed by identity, so order differences between the metadata
/// and metadata-free paths do not register as mismatches. `key` extracts the identity of an item
/// (e.g. a candidate hash). Aborts the process on any mismatch, reporting `(block, read, old, new)`.
pub fn compare_set<T, K, F>(block: H256, read: &str, old: &[T], new: &[T], key: F)
where
	T: PartialEq + Debug,
	K: Ord + Debug,
	F: Fn(&T) -> K,
{
	let old_map: BTreeMap<K, &T> = old.iter().map(|v| (key(v), v)).collect();
	let new_map: BTreeMap<K, &T> = new.iter().map(|v| (key(v), v)).collect();
	if old_map != new_map {
		report_and_abort(block, read, &format!("{old_map:?}"), &format!("{new_map:?}"));
	}
	// Two empty sets compare equal without proving the decoder, so only a populated comparison
	// counts as exercising the read.
	if !old.is_empty() || !new.is_empty() {
		mark(read);
	}
}

fn report_and_abort(block: H256, read: &str, old: &str, new: &str) -> ! {
	error!(
		"shadow-decode mismatch at block {block:?} for read `{read}`:\n  metadata      = {old}\n  metadata-free = {new}"
	);
	std::process::exit(1);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn renders_ok_and_pending_reads() {
		let mut counts = [0u64; EXPECTED_READS.len()];
		counts[0] = 3;
		let out = render_checklist(&counts);
		assert!(out.starts_with(&format!("shadow-decode checklist: 1/{} reads exercised", EXPECTED_READS.len())));
		assert!(out.contains("ok      block_header (3)"));
		assert!(out.contains("pending block_number"));
	}

	#[test]
	fn renders_logfmt_line() {
		let mut counts = [0u64; EXPECTED_READS.len()];
		counts[0] = 3;
		let out = render_logfmt(&counts);
		assert!(out.starts_with(&format!("shadow_checklist exercised=1 total={}", EXPECTED_READS.len())));
		assert!(out.contains(" block_header=3"));
		assert!(out.contains(" block_number=0"));
		assert!(!out.contains('\n'));
	}

	#[test]
	fn marks_only_meaningful_comparisons() {
		let block = H256::from([0u8; 32]);
		let empty: [u32; 0] = [];
		compare_set(block, "candidate_events", &empty, &empty, |v| *v);
		compare(block, "block_timestamp", &1u64, &1u64);
		compare_set(block, "disputes_initiated", &[7u32], &[7u32], |v| *v);

		let list = checklist().lock().unwrap();
		let idx = |read: &str| EXPECTED_READS.iter().position(|&expected| expected == read).unwrap();
		assert_eq!(list.counts[idx("candidate_events")], 0);
		assert!(list.counts[idx("block_timestamp")] > 0);
		assert!(list.counts[idx("disputes_initiated")] > 0);
	}
}
