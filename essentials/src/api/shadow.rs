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

use crate::types::H256;
use log::error;
use std::{collections::BTreeMap, fmt::Debug};

/// Compares the metadata decode (`old`) against the metadata-free decode (`new`) for a single
/// scalar read at `block`. Aborts the process on any mismatch, reporting `(block, read, old, new)`.
pub fn compare<T: PartialEq + Debug>(block: H256, read: &str, old: &T, new: &T) {
	if old != new {
		report_and_abort(block, read, &format!("{old:?}"), &format!("{new:?}"));
	}
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
}

fn report_and_abort(block: H256, read: &str, old: &str, new: &str) -> ! {
	error!(
		"shadow-decode mismatch at block {block:?} for read `{read}`:\n  metadata      = {old}\n  metadata-free = {new}"
	);
	std::process::exit(1);
}
