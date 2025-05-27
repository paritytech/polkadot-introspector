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

//! Implementation of the introspection using RocksDB

use super::{DBIter, IntrospectorKvdb};
use color_eyre::{Result, eyre::eyre};
use rocksdb::{DB, Options as RocksdbOptions};
use std::path::{Path, PathBuf};

pub struct IntrospectorRocksDB {
	inner: DB,
	columns: Vec<String>,
	read_only: bool,
	path: PathBuf,
}

const DEFAULT_COLUMN: &str = "default";

impl IntrospectorKvdb for IntrospectorRocksDB {
	fn new(path: &std::path::Path) -> Result<Self> {
		let mut cf_opts = RocksdbOptions::default();
		cf_opts.set_allow_mmap_reads(true);
		cf_opts.set_dump_malloc_stats(true);
		let mut columns = DB::list_cf(&cf_opts, path)?;
		// Always ignore default column to be compatible with ParityDB
		columns
			.iter()
			.position(|elt| elt == DEFAULT_COLUMN)
			.map(|default_column_pos| columns.remove(default_column_pos));
		let db = DB::open_cf_for_read_only(&cf_opts, path, columns.clone(), false)?;
		Ok(Self { inner: db, columns, read_only: true, path: path.into() })
	}

	fn list_columns(&self) -> color_eyre::Result<&Vec<String>> {
		Ok(&self.columns)
	}

	fn iter_values(&self, column: &str) -> Result<DBIter> {
		let mut iter_config = rocksdb::ReadOptions::default();
		// Do not cache values we read
		iter_config.fill_cache(false);
		// Optimize for iterations
		iter_config.set_readahead_size(4_194_304);
		// We don't care about checksums in this tool
		iter_config.set_verify_checksums(false);
		// We never iterate backwards
		iter_config.set_tailing(true);
		// We never need to store elements when iterating
		iter_config.set_pin_data(false);
		let cf_handle = self
			.inner
			.cf_handle(column)
			.ok_or_else(|| eyre!("invalid column: {}", column))?;
		let mut iter = self.inner.raw_iterator_cf_opt(cf_handle, iter_config);
		iter.seek_to_first();
		Ok(Box::new(std::iter::from_fn(move || {
			if !iter.valid() {
				None
			} else if let Some((key, value)) = iter.item() {
				let ret = Some((Box::from(key), Box::from(value)));
				iter.next();
				ret
			} else {
				None
			}
		})))
	}

	fn prefixed_iter_values(&self, column: &str, prefix: &str) -> Result<DBIter> {
		let cf_handle = self
			.inner
			.cf_handle(column)
			.ok_or_else(|| eyre!("invalid column: {}", column))?;
		// TODO: rocksdb does not support iterators with prefixes and options?
		let mut iter = self.inner.prefix_iterator_cf(cf_handle, prefix);
		Ok(Box::new(std::iter::from_fn(move || {
			if let Some(Ok((key, value))) = iter.next() { Some((key, value)) } else { None }
		})))
	}

	fn read_only(&self) -> bool {
		self.read_only
	}

	fn get_db_path(&self) -> &Path {
		self.path.as_path()
	}

	fn write_iter<I, K, V>(&self, column: &str, iter: I) -> Result<()>
	where
		I: IntoIterator<Item = (K, V)>,
		K: AsRef<[u8]>,
		V: AsRef<[u8]>,
	{
		if self.read_only {
			return Err(eyre!("cannot write a read-only database"))
		}

		let cf_handle = self
			.inner
			.cf_handle(column)
			.ok_or_else(|| eyre!("invalid column: {}", column))?;

		for (key, value) in iter {
			self.inner
				.put_cf(cf_handle, key, value)
				.map_err(|e| eyre!("error putting the key: {:?}", e))?;
		}

		Ok(())
	}

	fn new_dumper<D: IntrospectorKvdb>(input: &D, output_path: &std::path::Path) -> Result<Self> {
		let mut cf_opts = RocksdbOptions::default();
		let columns = input.list_columns()?.clone();
		cf_opts.create_if_missing(true);
		cf_opts.create_missing_column_families(true);

		let db = DB::open_cf(&cf_opts, output_path, &columns)?;
		Ok(IntrospectorRocksDB { inner: db, columns, read_only: false, path: output_path.into() })
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;
	use std::path::Path;

	pub fn new_test_rocks_db(output_path: &Path, num_columns: usize) -> IntrospectorRocksDB {
		let mut cf_opts = RocksdbOptions::default();
		let mut columns: Vec<String> = vec![];

		for i in 0..num_columns {
			columns.push(format!("col{}", i));
		}

		cf_opts.create_if_missing(true);
		cf_opts.create_missing_column_families(true);

		let db = DB::open_cf(&cf_opts, output_path, &columns).unwrap();
		IntrospectorRocksDB { inner: db, columns, read_only: false, path: output_path.into() }
	}
}
