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
//! Ephemeral/persistent in memory storage facilities for on-chain/off-chain data.
use std::{
	borrow::Borrow,
	collections::{BTreeMap, HashMap},
	hash::Hash,
	sync::Arc,
	time::Duration,
};

pub type BlockNumber = u32;

/// A type to identify the data source.
pub enum RecordSource {
	/// For onchain data.
	Onchain,
	/// For offchain data.
	Offchain,
}

/// A type to represent record timing information.
pub struct RecordTime {
	block_number: BlockNumber,
	timestamp: Option<Duration>,
}

/// An generic storage entry representation
/// TODO: create methods to create storage entries and redeem the inner data.
pub struct StorageEntry {
	/// The source of the data.
	record_source: RecordSource,
	/// Time index when data was recorded.
	/// All entries will have a block number. For offchain data, this is estimated based on the
	/// timestamp, or otherwise it will be set to the latest known block.
	record_time: RecordTime,
	/// The actual scale encoded data.
	data: Vec<u8>,
}

/// A required trait to implement for storing records.
pub trait StorageInfo {
	/// Returns the source of the data.
	fn source(&self) -> RecordSource;
	/// Returns the time when the data was recorded.
	fn time(&self) -> RecordTime;
}

/// Dummy impl to allow retrieveal of scale encoded values from storage.
/// After the value is decoded, the concrete type provides the real impl.
impl StorageInfo for Vec<u8> {
	/// Returns the source of the data.
	fn source(&self) -> RecordSource {
		RecordSource::Offchain
	}
	/// Returns the time when the data was recorded.
	fn time(&self) -> RecordTime {
		RecordTime { block_number: 0, timestamp: None }
	}
}

impl RecordTime {
	fn block_number(&self) -> BlockNumber {
		self.block_number
	}

	fn timestamp(&self) -> Option<Duration> {
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
pub struct RecordsStorage<K: Hash + Clone, EphemeralValue> {
	/// The configuration.
	config: RecordsStorageConfig,
	/// The last block number we've seen. Used to index the storage of all entries.
	last_block: Option<BlockNumber>,
	/// Elements with no expire date.
	persistent_records: BTreeMap<BlockNumber, HashMap<K, Arc<EphemeralValue>>>,
	/// Elements with expire dates.
	ephemeral_records: BTreeMap<BlockNumber, HashMap<K, Arc<EphemeralValue>>>,
	/// Direct mapping to values.
	direct_records: HashMap<K, Arc<EphemeralValue>>,
}

impl<K: Hash + Clone + Eq, EphemeralValue: StorageInfo + Clone> RecordsStorage<K, EphemeralValue> {
	/// Creates a new storage with the specified config
	pub fn new(config: RecordsStorageConfig) -> Self {
		let persistent_records = BTreeMap::new();
		let ephemeral_records = BTreeMap::new();
		let direct_records = HashMap::new();
		Self { config, last_block: None, persistent_records, ephemeral_records, direct_records }
	}

	/// Inserts a record in ephemeral storage.
	// TODO: must fail for values with blocks below the pruning threshold.
	pub fn insert(&mut self, key: K, value: EphemeralValue) {
		if self.direct_records.contains_key(&key) {
			return
		}
		let value = Arc::new(value);
		let block_number = value.time().block_number();
		self.last_block = Some(block_number);
		self.direct_records.insert(key.clone(), value.clone());

		self.ephemeral_records
			.entry(block_number)
			.or_insert(Default::default())
			.insert(key.clone(), value);

		self.prune();
	}

	// Prune all entries which are older than `self.config.max_blocks` vs current block.
	pub fn prune(&mut self) {
		let block_count = self.ephemeral_records.len();
		// Check if the chain has advanced more than maximum allowed blocks.
		if block_count > self.config.max_blocks {
			// Prune all entries at oldest block
			let oldest_block = {
				let (oldest_block, entries) = self.ephemeral_records.iter().next().unwrap();
				for (key, _value) in entries.into_iter() {
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
	pub fn get<Q: ?Sized>(&self, key: &Q) -> Option<EphemeralValue>
	where
		K: Borrow<Q>,
		Q: Hash + Eq,
	{
		if let Some(value) = self.direct_records.get(key).cloned() {
			Some((*value).clone())
		} else {
			None
		}
	}

	/// Size of the storage
	pub fn len(&self) -> usize {
		self.direct_records.len()
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

		st.insert("key1".to_owned(), 1);
		st.insert("key100".to_owned(), 2);

		assert_eq!(st.get("key1").unwrap(), 1);
		assert_eq!(st.get("key100").unwrap(), 2);
		assert_eq!(st.get("key2"), None);

		// This insert prunes prev entries at block #1
		st.insert("key2".to_owned(), 100);
		assert_eq!(st.get("key2").unwrap(), 100);

		assert_eq!(st.get("key1"), None);
		assert_eq!(st.get("key100"), None);
	}

	#[test]
	fn test_prune() {
		let mut st = RecordsStorage::new(RecordsStorageConfig { max_blocks: 2 });

		for idx in 0..1000 {
			st.insert(idx, idx);
			st.insert(idx, idx);
		}

		// 10 keys per block * 2 max blocks.
		assert_eq!(st.len(), 20);
	}
}
