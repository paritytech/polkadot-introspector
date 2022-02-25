// Copyright 2022 Parity Technologies (UK) Ltd.
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

//! Stores candidates records in a buffer allowing to prune old candidates
//! and optionally maintain the persistent config
use std::{
	borrow::Borrow,
	collections::{HashMap, VecDeque},
	hash::Hash,
	time::Duration,
};
use typed_builder::TypedBuilder;

/// Storage configuration
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RecordsStorageConfig {
	/// Maximum records to store, if None, then no limit is defined
	pub max_records: Option<usize>,
	/// Maximum ttl for elements in seconds
	pub max_ttl: Option<usize>,
}

/// A simple trait required for storage entry items
pub trait StorageEntry {
	fn get_time(&self) -> Duration;
}

/// Entry used to store elements in a buffer with expiration
#[derive(TypedBuilder)]
struct ExpiryEntry<T: Hash> {
	/// A creation time for this record
	timestamp: Duration,
	/// Hash that must be corresponding to the records hash table
	hash: T,
}

/// Persistent in-memory storage with expiration and max ttl
/// This storage has also an associative component allowing to get an element
/// by hash
pub struct RecordsStorage<K: Hash + Clone, V: StorageEntry> {
	/// Config for the expiration
	cfg: RecordsStorageConfig,
	/// Elements stored
	records: HashMap<K, V>,
	/// Traces for removal
	records_expiry_trace: VecDeque<ExpiryEntry<K>>,
}

impl<K: Hash + Clone + Eq, V: StorageEntry> RecordsStorage<K, V> {
	/// Creates a new storage with the expiration config defined
	pub fn new(cfg: RecordsStorageConfig) -> Self {
		let initial_capacity = cfg.max_records.unwrap_or_default();
		let records = HashMap::<K, V>::with_capacity(initial_capacity);
		let expiry_trace = VecDeque::<ExpiryEntry<K>>::with_capacity(initial_capacity);
		Self { cfg, records, records_expiry_trace: expiry_trace }
	}

	/// Inserts a record in storage
	// TODO: must fail on duplicate + check that timestamp >= front.timestamp
	pub fn insert(&mut self, key: K, value: V) -> Option<V> {
		self.records_expiry_trace.push_front(
			ExpiryEntry::<K>::builder()
				.timestamp(<V as StorageEntry>::get_time(&value))
				.hash(key.clone())
				.build(),
		);
		let existing = self.records.insert(key, value);

		self.maybe_expire_elements();

		existing
	}

	/// Gets a value with a specific key
	// TODO: think if we need to check max_ttl and initiate expiry on `get` method
	pub fn get<Q: ?Sized>(&self, key: &Q) -> Option<&V>
	where
		K: Borrow<Q>,
		Q: Hash + Eq,
	{
		self.records.get(key)
	}
	/// Gets a value with a specific key as a mutable reference
	pub fn get_mut<Q: ?Sized>(&mut self, key: &Q) -> Option<&mut V>
	where
		K: Borrow<Q>,
		Q: Hash + Eq,
	{
		self.records.get_mut(key)
	}

	/// Size of the storage
	pub fn len(&self) -> usize {
		self.records.len()
	}

	/// Get access to the underlying hash map (for testing, for example)
	pub fn records(&self) -> &HashMap<K, V> {
		&self.records
	}

	// Internal expiration method
	fn maybe_expire_elements(&mut self) -> usize {
		let mut expired = 0;
		if let Some(max_records) = self.cfg.max_records {
			while self.records_expiry_trace.len() > max_records {
				let elt = self.records_expiry_trace.pop_back();

				match elt {
					Some(elt) => self.records.remove(&elt.hash),
					_ => break, // No more entries
				};
				expired += 1;
			}
		}

		if let Some(max_ttl) = self.cfg.max_ttl {
			// We assume that elements on the back of the vector are the most old
			let last_known: Duration = self
				.records_expiry_trace
				.front()
				.map_or(Duration::ZERO, |entry| entry.timestamp);
			if !last_known.is_zero() {
				loop {
					let elt = self.records_expiry_trace.back();

					match elt {
						Some(elt) => {
							let threshold = elt.timestamp.saturating_add(Duration::from_secs(max_ttl as u64));
							if threshold < last_known {
								self.records.remove(&elt.hash);
								self.records_expiry_trace.pop_back();
								expired += 1;
							} else {
								// Last known found
								break;
							}
						},
						// No elements left
						_ => break,
					}
				}
			}
		}

		expired
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	impl StorageEntry for i32 {
		fn get_time(&self) -> Duration {
			Duration::from_secs(*self as u64)
		}
	}
	#[test]
	fn test_evict_by_size() {
		let mut st = RecordsStorage::<String, i32>::new(RecordsStorageConfig { max_records: Some(1), max_ttl: None });

		assert!(st.insert("test".to_owned(), 1).is_none());
		assert_eq!(st.len(), 1);
		assert_eq!(*st.get("test").unwrap(), 1);
		assert_eq!(*st.get_mut("test").unwrap(), 1);
		assert_eq!(*st.records().get("test").unwrap(), 1);
		assert!(st.insert("test1".to_owned(), 2).is_none());
		// Ensure eviction
		assert_eq!(st.len(), 1);
		// Ensure that we have left only last entry
		assert!(st.get("test").is_none());
		assert_eq!(*st.get("test1").unwrap(), 2);
	}
	#[test]
	fn test_evict_by_ttl() {
		let mut st = RecordsStorage::<String, i32>::new(RecordsStorageConfig { max_records: None, max_ttl: Some(1) });

		assert!(st.insert("test".to_owned(), 1).is_none());
		assert_eq!(st.len(), 1);
		assert_eq!(*st.get("test").unwrap(), 1);

		assert!(st.insert("test1".to_owned(), 2).is_none());
		assert_eq!(st.len(), 2);
		assert_eq!(*st.get("test1").unwrap(), 2);

		// This will trigger eviction by ttl
		assert!(st.insert("test2".to_owned(), 3).is_none());
		assert_eq!(st.len(), 2);
		// Ensure that we have left only last two entries
		assert!(st.get("test").is_none());
		assert_eq!(*st.get("test1").unwrap(), 2);
		assert_eq!(*st.get("test2").unwrap(), 3);

		// This will not trigger eviction
		assert!(st.insert("test3".to_owned(), 3).is_none());
		assert_eq!(st.len(), 3);
		// Ensure that we have left only last two entries
		assert!(st.get("test").is_none());
		assert_eq!(*st.get("test1").unwrap(), 2);
		assert_eq!(*st.get("test2").unwrap(), 3);
		assert_eq!(*st.get("test3").unwrap(), 3);

		// This will evict all entries with the exception of the one inserted
		assert!(st.insert("test4".to_owned(), 5).is_none());
		assert_eq!(st.len(), 1);
		assert!(st.get("test1").is_none());
		assert!(st.get("test2").is_none());
		assert!(st.get("test3").is_none());
		assert_eq!(*st.get("test4").unwrap(), 5);
	}
}
