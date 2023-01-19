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
//! The storage is designed to store **unique** keys and will return errors when
//! trying to insert already existing values.
//! To update the existing entries, this API users should use the `replace` method.
//! Values are stored as scale encoded byte chunks and are **copied** on calling of the
//! `get` method. This is done for the API simplicity as the performance is not a
//! goal here.
#![allow(dead_code)]

use crate::eyre;
use codec::{Decode, Encode};
use std::{
	borrow::Borrow,
	collections::{BTreeMap, HashMap, HashSet},
	fmt::Debug,
	hash::Hash,
	time::Duration,
};

pub type BlockNumber = u32;

/// A type to identify the record type
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum RecordType {
	/// For onchain data.
	Onchain,
	/// For offchain data.
	Offchain,
	/// For persistent data (not pruned automatically)
	Persistent,
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
	/// The type of the data.
	record_type: RecordType,
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
		StorageEntry { record_type: RecordType::Onchain, record_time, data: data.encode() }
	}

	/// Creates a new storage entry for onchain data.
	pub fn new_offchain<T: Encode>(record_time: RecordTime, data: T) -> StorageEntry {
		StorageEntry { record_type: RecordType::Offchain, record_time, data: data.encode() }
	}

	/// Creates a new persistent storage entry
	pub fn new_persistent<T: Encode>(record_time: RecordTime, data: T) -> StorageEntry {
		StorageEntry { record_type: RecordType::Persistent, record_time, data: data.encode() }
	}

	/// Converts a storage entry into it's original type by decoding from scale codec
	pub fn into_inner<T: Decode>(self) -> color_eyre::Result<T> {
		T::decode(&mut self.data.as_slice()).map_err(|e| eyre!("decode error: {:?}", e))
	}
}

/// A required trait to implement for storing records.
pub trait StorageInfo {
	/// Returns the type of the data.
	fn record_type(&self) -> RecordType;
	/// Returns the time when the data was recorded.
	fn time(&self) -> RecordTime;
}

impl StorageInfo for StorageEntry {
	/// Returns the source of the data.
	fn record_type(&self) -> RecordType {
		self.record_type
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

/// This trait defines basic functions for the storage
pub trait RecordsStorage<K> {
	/// Creates a new storage with the specified config
	fn new(config: RecordsStorageConfig) -> Self;
	/// Inserts a record in ephemeral storage. This method does not overwrite
	/// records and returns an error in case of a duplicate entry.
	fn insert(&mut self, key: K, entry: StorageEntry) -> color_eyre::Result<()>;
	/// Replaces an **existing** entry in storage with another entry. The existing entry is returned, otherwise,
	/// no record is inserted and `None` is returned to indicate an error
	fn replace<Q: ?Sized + Hash + Eq>(&mut self, key: &Q, entry: StorageEntry) -> Option<StorageEntry>
	where
		K: Borrow<Q>;
	/// Prunes all entries which are older than `self.config.max_blocks` vs current block.
	fn prune(&mut self);
	/// Gets a value with a specific key (this method copies a value stored)
	fn get<Q: ?Sized + Hash + Eq>(&self, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>;
	/// Delete a value with a specific key (this method copies returns a copy of the deleted element if found)
	fn delete<Q: ?Sized + Hash + Eq>(&mut self, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>;
	/// Size of the storage
	fn len(&self) -> usize;
	/// Returns all keys in the storage
	fn keys(&self) -> Vec<K>;
}

/// Persistent in-memory storage with expiration and max TTL
/// This storage has also an associative component allowing to get an element
/// by hash
pub struct HashedPlainRecordsStorage<K: Hash + Clone> {
	/// The configuration.
	config: RecordsStorageConfig,
	/// The last block number we've seen. Used to index the storage of all entries.
	last_block: Option<BlockNumber>,
	/// Elements with expire dates.
	ephemeral_records: BTreeMap<BlockNumber, HashSet<K>>,
	/// Direct mapping to values.
	direct_records: HashMap<K, StorageEntry>,
}

impl<K> RecordsStorage<K> for HashedPlainRecordsStorage<K>
where
	K: Hash + Clone + Eq + Debug,
{
	fn new(config: RecordsStorageConfig) -> Self {
		let ephemeral_records = BTreeMap::new();
		let direct_records = HashMap::new();
		Self { config, last_block: None, ephemeral_records, direct_records }
	}

	// TODO: must fail for values with blocks below the pruning threshold.
	fn insert(&mut self, key: K, entry: StorageEntry) -> color_eyre::Result<()> {
		if self.direct_records.contains_key(&key) {
			return Err(eyre!("duplicate key: {:?}", key))
		}
		let block_number = entry.time().block_number();
		self.last_block = Some(block_number);
		let is_persistent = entry.record_type == RecordType::Persistent;
		self.direct_records.insert(key.clone(), entry);

		if !is_persistent {
			self.ephemeral_records
				.entry(block_number)
				.or_insert_with(Default::default)
				.insert(key);
		}

		self.prune();
		Ok(())
	}

	fn replace<Q: ?Sized + Hash + Eq>(&mut self, key: &Q, entry: StorageEntry) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
	{
		if !self.direct_records.contains_key(key) {
			None
		} else {
			let record = self.direct_records.get_mut(key).unwrap();
			Some(std::mem::replace(record, entry))
		}
	}

	fn prune(&mut self) {
		let block_count = self.ephemeral_records.len();
		// Check if the chain has advanced more than maximum allowed blocks.
		if block_count > self.config.max_blocks {
			// Prune all entries at oldest block
			let oldest_block = {
				let (oldest_block, entries) = self.ephemeral_records.iter().next().unwrap();
				for key in entries.iter() {
					self.direct_records.remove(key);
				}

				*oldest_block
			};

			// Finally remove the block mapping
			self.ephemeral_records.remove(&oldest_block);
		}
	}

	// TODO: think if we need to check max_ttl and initiate expiry on `get` method
	fn get<Q: ?Sized + Hash + Eq>(&self, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
	{
		self.direct_records.get(key).cloned()
	}

	fn delete<Q: ?Sized + Hash + Eq>(&mut self, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
	{
		self.direct_records.remove(key)
	}

	fn len(&self) -> usize {
		self.direct_records.len()
	}

	fn keys(&self) -> Vec<K> {
		self.direct_records.keys().cloned().collect()
	}
}

/// This trait is used to define a storage that can store items organized in prefixes.
/// Prefixes are used to group elements by some characteristic. For example, to get
/// elements that belong to some particular parachain.
pub trait PrefixedRecordsStorage<K, P> {
	/// Insert a prefixed entry to the storage, returns a usage Error if trying to insert a duplicate
	/// key + prefix
	fn insert_prefix(&mut self, prefix: P, key: K, entry: StorageEntry) -> color_eyre::Result<()>;
	/// Replaces a prefixed entry in the storage, both prefix and a key must exist,
	/// returns the old entry on success and None on error
	fn replace_prefixed<Q: ?Sized + Hash + Eq, PQ: ?Sized + Hash + Eq>(
		&mut self,
		prefix: &PQ,
		key: &Q,
		entry: StorageEntry,
	) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
		P: Borrow<PQ>;
	/// Get a key using specific prefix along with the key
	fn get_prefix<Q: ?Sized + Hash + Eq, PQ: ?Sized + Hash + Eq>(&self, prefix: &PQ, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
		P: Borrow<PQ>;
	/// Remove a key using specific prefix along with the key
	fn delete_prefix<Q: ?Sized + Hash + Eq, PQ: ?Sized + Hash + Eq>(
		&mut self,
		prefix: &PQ,
		key: &Q,
	) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
		P: Borrow<PQ>;
	/// Get keys for a specific prefix
	fn prefixed_keys<PQ: ?Sized + Hash + Eq>(&self, prefix: &PQ) -> Vec<K>
	where
		P: Borrow<PQ>;
	/// Get all prefixes from a storage
	fn prefixes(&self) -> Vec<P>;
}

/// Prefixed storage is distinct as it organise data stored using prefixes,
/// for example to store entries for different parachains and relay parents
/// The keys should be unique in all distinct prefixes, that can be
/// guaranteed by assuming that K is a cryptographic hash
/// This data structure is intended to work with a small and limited number of
/// prefixes, as it will likely perform a hash lookup per each prefix
/// when searching for a key in non-prefixed matter
pub struct HashedPrefixedRecordsStorage<K: Hash + Clone, P: Hash + Clone> {
	/// The configuration.
	config: RecordsStorageConfig,
	/// The last block number we've seen. Used to index the storage of all entries.
	last_block: Option<BlockNumber>,
	/// Elements with expire dates.
	ephemeral_records: BTreeMap<BlockNumber, HashSet<K>>,
	/// Direct mapping to values.
	prefixed_records: HashMap<P, HashMap<K, StorageEntry>>,
}

impl<K, P> RecordsStorage<K> for HashedPrefixedRecordsStorage<K, P>
where
	K: Hash + Clone + Eq + Debug,
	P: Hash + Clone + Eq + Debug,
{
	fn new(config: RecordsStorageConfig) -> Self {
		let ephemeral_records = BTreeMap::new();
		let prefixed_records = HashMap::new();
		Self { config, last_block: None, ephemeral_records, prefixed_records }
	}

	// We cannot insert non prefixed key into a prefixed storage
	fn insert(&mut self, key: K, _: StorageEntry) -> color_eyre::Result<()> {
		return Err(eyre!("trying to insert key with no prefix to the prefixed storage: {:?}", key))
	}

	fn replace<Q: ?Sized + Hash + Eq>(&mut self, key: &Q, entry: StorageEntry) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
	{
		for direct_map in self.prefixed_records.values_mut() {
			if let Some(record) = direct_map.get_mut(key) {
				return Some(std::mem::replace(record, entry))
			}
		}

		None
	}

	fn prune(&mut self) {
		let block_count = self.ephemeral_records.len();
		// Check if the chain has advanced more than maximum allowed blocks.
		if block_count > self.config.max_blocks {
			// Prune all entries at oldest block
			let oldest_block = {
				let (oldest_block, entries) = self.ephemeral_records.iter().next().unwrap();
				for key in entries.iter() {
					for direct_map in self.prefixed_records.values_mut() {
						direct_map.remove(key);
					}
				}

				*oldest_block
			};

			// Finally remove the block mapping
			self.ephemeral_records.remove(&oldest_block);
		}
	}

	// TODO: think if we need to check max_ttl and initiate expiry on `get` method
	fn get<Q: ?Sized + Hash + Eq>(&self, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
	{
		self.prefixed_records
			.iter()
			.find_map(|(_, direct_map)| direct_map.get(key).cloned())
	}

	fn delete<Q: ?Sized + Hash + Eq>(&mut self, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
	{
		for (_, direct_map) in self.prefixed_records.iter_mut() {
			if direct_map.contains_key(key) {
				return direct_map.remove(key)
			}
		}

		None
	}

	fn len(&self) -> usize {
		self.prefixed_records.values().map(|direct_map| direct_map.len()).sum()
	}

	fn keys(&self) -> Vec<K> {
		self.prefixed_records
			.values()
			.flat_map(|direct_map| direct_map.keys())
			.cloned()
			.collect()
	}
}

impl<K, P> PrefixedRecordsStorage<K, P> for HashedPrefixedRecordsStorage<K, P>
where
	K: Hash + Clone + Eq + Debug,
	P: Hash + Clone + Eq + Debug,
{
	fn insert_prefix(&mut self, prefix: P, key: K, entry: StorageEntry) -> color_eyre::Result<()> {
		let direct_storage = self.prefixed_records.entry(prefix).or_default();
		if direct_storage.contains_key(&key) {
			return Err(eyre!("duplicate key: {:?}", key))
		}
		let block_number = entry.time().block_number();
		self.last_block = Some(block_number);
		direct_storage.insert(key.clone(), entry);

		self.ephemeral_records
			.entry(block_number)
			.or_insert_with(Default::default)
			.insert(key);

		self.prune();
		Ok(())
	}

	fn replace_prefixed<Q: ?Sized + Hash + Eq, PQ: ?Sized + Hash + Eq>(
		&mut self,
		prefix: &PQ,
		key: &Q,
		entry: StorageEntry,
	) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
		P: Borrow<PQ>,
	{
		let direct_storage = self.prefixed_records.get_mut(prefix)?;
		if !direct_storage.contains_key(key) {
			None
		} else {
			let record = direct_storage.get_mut(key).unwrap();
			Some(std::mem::replace(record, entry))
		}
	}

	fn get_prefix<Q: ?Sized + Hash + Eq, PQ: ?Sized + Hash + Eq>(&self, prefix: &PQ, key: &Q) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
		P: Borrow<PQ>,
	{
		if let Some(direct_storage) = self.prefixed_records.get(prefix) {
			return direct_storage.get(key).cloned()
		}

		None
	}

	fn delete_prefix<Q: ?Sized + Hash + Eq, PQ: ?Sized + Hash + Eq>(
		&mut self,
		prefix: &PQ,
		key: &Q,
	) -> Option<StorageEntry>
	where
		K: Borrow<Q>,
		P: Borrow<PQ>,
	{
		if let Some(direct_storage) = self.prefixed_records.get_mut(prefix) {
			return direct_storage.remove(key)
		}

		None
	}

	fn prefixed_keys<PQ: ?Sized + Hash + Eq>(&self, prefix: &PQ) -> Vec<K>
	where
		P: Borrow<PQ>,
	{
		if let Some(direct_storage) = self.prefixed_records.get(prefix) {
			direct_storage.keys().cloned().collect()
		} else {
			vec![]
		}
	}

	fn prefixes(&self) -> Vec<P> {
		self.prefixed_records.keys().cloned().collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	impl StorageInfo for u32 {
		/// Returns the source of the data.
		fn record_type(&self) -> RecordType {
			RecordType::Onchain
		}

		/// Returns the time when the data was recorded.
		fn time(&self) -> RecordTime {
			RecordTime { block_number: self / 10, timestamp: None }
		}
	}

	#[test]
	fn test_it_works() {
		let mut st = HashedPlainRecordsStorage::new(RecordsStorageConfig { max_blocks: 1 });

		st.insert("key1".to_owned(), StorageEntry::new_onchain(1.into(), 1_u32))
			.unwrap();
		st.insert("key100".to_owned(), StorageEntry::new_offchain(1.into(), 2_u32))
			.unwrap();

		let a = st.get("key1").unwrap();
		assert_eq!(a.record_type, RecordType::Onchain);
		assert_eq!(a.into_inner::<u32>().unwrap(), 1);

		let b = st.get("key100").unwrap();
		assert_eq!(b.record_type, RecordType::Offchain);
		assert_eq!(b.into_inner::<u32>().unwrap(), 2);
		assert_eq!(st.get("key2"), None);

		// This insert prunes prev entries at block #1
		st.insert("key2".to_owned(), StorageEntry::new_onchain(100.into(), 100_u32))
			.unwrap();
		assert_eq!(st.get("key2").unwrap().into_inner::<u32>().unwrap(), 100);

		assert_eq!(st.get("key1"), None);
		assert_eq!(st.get("key100"), None);

		// This insert prunes prev entries at block #100
		st.insert("key3".to_owned(), StorageEntry::new_persistent(200.into(), 100_u32))
			.unwrap();
		assert_eq!(st.get("key3").unwrap().into_inner::<u32>().unwrap(), 100);
		// Persistent record do not invalidate normal ones
		assert_eq!(st.get("key2").unwrap().into_inner::<u32>().unwrap(), 100);
		st.insert("key4".to_owned(), StorageEntry::new_onchain(300.into(), 100_u32))
			.unwrap();
		st.insert("key5".to_owned(), StorageEntry::new_onchain(400.into(), 100_u32))
			.unwrap();
		// Persistent key must persist, non-persistent should be wiped
		assert_eq!(st.get("key2"), None);
		assert_eq!(st.get("key3").unwrap().into_inner::<u32>().unwrap(), 100);
		assert_eq!(st.delete("key3").unwrap().into_inner::<u32>().unwrap(), 100);
		assert_eq!(st.get("key3"), None);
	}

	#[test]
	fn test_prune() {
		let mut st = HashedPlainRecordsStorage::new(RecordsStorageConfig { max_blocks: 2 });

		for idx in 0..1000 {
			st.insert(idx, StorageEntry::new_onchain((idx / 10).into(), idx)).unwrap();
		}

		// 10 keys per block * 2 max blocks.
		assert_eq!(st.len(), 20);
	}

	#[test]
	fn test_duplicate() {
		let mut st = HashedPlainRecordsStorage::new(RecordsStorageConfig { max_blocks: 1 });

		st.insert("key".to_owned(), StorageEntry::new_onchain(1.into(), 1)).unwrap();
		// Cannot overwrite
		assert!(st.insert("key".to_owned(), StorageEntry::new_onchain(1.into(), 2)).is_err());
		let a = st.get("key").unwrap();
		assert_eq!(a.into_inner::<u32>().unwrap(), 1);
		// Can replace
		st.replace("key", StorageEntry::new_onchain(1.into(), 2)).unwrap();
		let a = st.get("key").unwrap();
		assert_eq!(a.into_inner::<u32>().unwrap(), 2);
	}

	#[test]
	fn test_prefixes() {
		let mut st = HashedPrefixedRecordsStorage::new(RecordsStorageConfig { max_blocks: 1 });

		st.insert_prefix("aba".to_owned(), "abaa".to_owned(), StorageEntry::new_onchain(1.into(), 1))
			.unwrap();
		st.insert_prefix("aba".to_owned(), "aba".to_owned(), StorageEntry::new_onchain(1.into(), 1))
			.unwrap();
		st.insert_prefix("abc".to_owned(), "aba".to_owned(), StorageEntry::new_onchain(1.into(), 1))
			.unwrap();
		st.insert_prefix("abc".to_owned(), "abaa".to_owned(), StorageEntry::new_onchain(1.into(), 1))
			.unwrap();
		st.insert_prefix("abcd".to_owned(), "aba".to_owned(), StorageEntry::new_onchain(1.into(), 1))
			.unwrap();

		let mut prefixed_search = st.prefixed_keys("aba");
		assert_eq!(prefixed_search.len(), 2);
		prefixed_search.sort();
		assert_eq!(prefixed_search[0], "aba");
		assert_eq!(prefixed_search[1], "abaa");
		// Single key with this prefix
		let prefixed_search = st.prefixed_keys("abcd");
		assert_eq!(prefixed_search.len(), 1);
		let prefixed_search = st.prefixed_keys("no");
		assert_eq!(prefixed_search.len(), 0);
	}
}
