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
//! Ephemeral in memory storage facilities for on-chain/off-chain data.
#![allow(dead_code)]

use crate::eyre;
use codec::{Decode, Encode};
use std::{
	borrow::Borrow,
	collections::{BTreeMap, HashMap},
	hash::Hash,
	sync::{Arc, Weak},
	time::Duration,
};

pub type BlockNumber = u32;

/// A type to identify the data source.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum RecordSource {
	/// For onchain data.
	Onchain,
	/// For offchain data.
	Offchain,
}

/// A type to represent record timing information.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub struct RecordTime {
	block_number: BlockNumber,
	timestamp: Option<Duration>,
}

impl From<BlockNumber> for RecordTime {
	fn from(block_number: BlockNumber) -> Self {
		let timestamp = None;
		RecordTime { block_number, timestamp }
	}
}

impl RecordTime {
	pub fn with_ts(block_number: BlockNumber, timestamp: Duration) -> Self {
		let timestamp = Some(timestamp);
		RecordTime { block_number, timestamp }
	}
}

/// An generic storage entry representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageEntry {
	/// The source of the data.
	record_source: RecordSource,
	/// Time index when data was recorded.
	/// All entries will have a block number. For offchain data, this is estimated based on the
	/// timestamp, or otherwise it needs to be set to the latest known block.
	record_time: RecordTime,
	/// The actual scale encoded data.
	data: Vec<u8>,
}

impl StorageEntry {
	/// Creates a new storage entry for onchain data.
	pub fn new_onchain<T: Encode>(record_time: RecordTime, data: T) -> StorageEntry {
		StorageEntry { record_source: RecordSource::Onchain, record_time, data: data.encode() }
	}

	/// Creates a new storage entry for onchain data.
	pub fn new_offchain<T: Encode>(record_time: RecordTime, data: T) -> StorageEntry {
		StorageEntry { record_source: RecordSource::Offchain, record_time, data: data.encode() }
	}

	/// Converts a storage entry into it's original type by decoding from scale codec
	pub fn into_inner<T: Decode>(self) -> color_eyre::Result<T> {
		T::decode(&mut self.data.as_slice()).map_err(|e| eyre!("decode error: {:?}", e))
	}
}

/// A required trait to implement for storing records.
pub trait StorageInfo {
	/// Returns the source of the data.
	fn source(&self) -> RecordSource;
	/// Returns the time when the data was recorded.
	fn time(&self) -> RecordTime;
}

impl StorageInfo for StorageEntry {
	/// Returns the source of the data.
	fn source(&self) -> RecordSource {
		self.record_source
	}
	/// Returns the time when the data was recorded.
	fn time(&self) -> RecordTime {
		self.record_time
	}
}

impl RecordTime {
	/// Returns the number of the block
	pub fn block_number(&self) -> BlockNumber {
		self.block_number
	}

	/// Returns timestamp of the record
	pub fn timestamp(&self) -> Option<Duration> {
		self.timestamp
	}
}

/// Storage configuration
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RecordsStorageConfig {
	/// Maximum number of blocks for which we keep storage entries.
	pub max_blocks: usize,
}

/// Persistent in-memory storage with expiration and max ttl
/// This storage has also an associative component allowing to get an element
/// by hash
pub struct RecordsStorage<K: Hash + Clone> {
	/// The configuration.
	config: RecordsStorageConfig,
	/// The last block number we've seen. Used to index the storage of all entries.
	last_block: Option<BlockNumber>,
	/// Elements with expire dates.
	ephemeral_records: BTreeMap<BlockNumber, HashMap<K, Weak<StorageEntry>>>,
	/// Direct mapping to values.
	direct_records: HashMap<K, Arc<StorageEntry>>,
}

impl<K: Hash + Clone + Eq> RecordsStorage<K> {
	/// Creates a new storage with the specified config
	pub fn new(config: RecordsStorageConfig) -> Self {
		let ephemeral_records = BTreeMap::new();
		let direct_records = HashMap::new();
		Self { config, last_block: None, ephemeral_records, direct_records }
	}

	/// Inserts a record in ephemeral storage.
	// TODO: must fail for values with blocks below the pruning threshold.
	pub fn insert(&mut self, key: K, entry: StorageEntry) {
		if self.direct_records.contains_key(&key) {
			return
		}
		let entry = Arc::new(entry);
		let block_number = entry.time().block_number();
		self.last_block = Some(block_number);
		self.direct_records.insert(key.clone(), entry.clone());

		self.ephemeral_records
			.entry(block_number)
			.or_insert_with(Default::default)
			.insert(key, Arc::downgrade(&entry));

		self.prune();
	}

	pub fn replace(&mut self, key: K, entry: StorageEntry) -> Option<StorageEntry> {
		if !self.direct_records.contains_key(&key) {
			None
		} else {
			let record = self.direct_records.get_mut(&key).unwrap();
			Some(std::mem::replace(Arc::make_mut(record), entry))
		}
	}

	// Prune all entries which are older than `self.config.max_blocks` vs current block.
	pub fn prune(&mut self) {
		let block_count = self.ephemeral_records.len();
		// Check if the chain has advanced more than maximum allowed blocks.
		if block_count > self.config.max_blocks {
			// Prune all entries at oldest block
			let oldest_block = {
				let (oldest_block, entries) = self.ephemeral_records.iter().next().unwrap();
				for (key, _) in entries.iter() {
					self.direct_records.remove(key);
				}

				*oldest_block
			};

			// Finally remove the block mapping
			self.ephemeral_records.remove(&oldest_block);
		}
	}

	/// Gets a value with a specific key
	// TODO: think if we need to check max_ttl and initiate expiry on `get` method
	pub fn get<Q: ?Sized>(&self, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
		Q: Hash + Eq,
	{
		self.direct_records.get(key).cloned().map(|value| (*value).clone())
	}

	/// Size of the storage
	pub fn len(&self) -> usize {
		self.direct_records.len()
	}

	/// Returns all keys in the storage
	pub fn keys(&self) -> Vec<K> {
		self.direct_records.keys().cloned().collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	impl StorageInfo for u32 {
		/// Returns the source of the data.
		fn source(&self) -> RecordSource {
			RecordSource::Onchain
		}

		/// Returns the time when the data was recorded.
		fn time(&self) -> RecordTime {
			RecordTime { block_number: self / 10, timestamp: None }
		}
	}

	#[test]
	fn test_it_works() {
		let mut st = RecordsStorage::new(RecordsStorageConfig { max_blocks: 1 });

		st.insert("key1".to_owned(), StorageEntry::new_onchain(1.into(), 1));
		st.insert("key100".to_owned(), StorageEntry::new_offchain(1.into(), 2));

		let a = st.get("key1").unwrap();
		assert_eq!(a.record_source, RecordSource::Onchain);
		assert_eq!(a.into_inner::<u32>().unwrap(), 1);

		let b = st.get("key100").unwrap();
		assert_eq!(b.record_source, RecordSource::Offchain);
		assert_eq!(b.into_inner::<u32>().unwrap(), 2);
		assert_eq!(st.get("key2"), None);

		// This insert prunes prev entries at block #1
		st.insert("key2".to_owned(), StorageEntry::new_onchain(100.into(), 100));
		assert_eq!(st.get("key2").unwrap().into_inner::<u32>().unwrap(), 100);

		assert_eq!(st.get("key1"), None);
		assert_eq!(st.get("key100"), None);
	}

	#[test]
	fn test_prune() {
		let mut st = RecordsStorage::new(RecordsStorageConfig { max_blocks: 2 });

		for idx in 0..1000 {
			st.insert(idx, StorageEntry::new_onchain((idx / 10).into(), idx));
			st.insert(idx, StorageEntry::new_onchain((idx / 10).into(), idx));
		}

		// 10 keys per block * 2 max blocks.
		assert_eq!(st.len(), 20);
	}

	#[test]
	fn test_duplicate() {
		let mut st = RecordsStorage::new(RecordsStorageConfig { max_blocks: 1 });

		st.insert("key".to_owned(), StorageEntry::new_onchain(1.into(), 1));
		// Cannot overwrite
		st.insert("key".to_owned(), StorageEntry::new_onchain(1.into(), 2));
		let a = st.get("key").unwrap();
		assert_eq!(a.into_inner::<u32>().unwrap(), 1);
		// Can replace
		st.replace("key".to_owned(), StorageEntry::new_onchain(1.into(), 2));
		let a = st.get("key").unwrap();
		assert_eq!(a.into_inner::<u32>().unwrap(), 2);
	}
}
