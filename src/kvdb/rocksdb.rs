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
use color_eyre::{eyre::eyre, Result};
use rocksdb::{IteratorMode, Options as RocksdbOptions, DB};

pub struct IntrospectorRocksDB {
	inner: DB,
	columns: Vec<String>,
	read_only: bool,
}

const DEFAULT_COLUMN: &str = "default";

impl IntrospectorKvdb for IntrospectorRocksDB {
	fn new(path: &str) -> Result<Self> {
		let cf_opts = RocksdbOptions::default();
		let mut columns = DB::list_cf(&cf_opts, path)?;
		// Always ignore default column to be compatible with ParityDB
		columns
			.iter()
			.position(|elt| elt == DEFAULT_COLUMN)
			.map(|default_column_pos| columns.remove(default_column_pos));
		let db = DB::open_cf_for_read_only(&cf_opts, path, columns.clone(), false)?;
		Ok(Self { inner: db, columns, read_only: true })
	}

	fn list_columns(&self) -> color_eyre::Result<&Vec<String>> {
		Ok(&self.columns)
	}

	fn iter_values(&self, column: &str) -> Result<DBIter> {
		let cf_handle = self
			.inner
			.cf_handle(column)
			.ok_or_else(|| eyre!("invalid column: {}", column))?;
		let iter = self.inner.iterator_cf(cf_handle, IteratorMode::Start);
		Ok(Box::new(iter))
	}

	fn prefixed_iter_values(&self, column: &str, prefix: &str) -> Result<DBIter> {
		let cf_handle = self
			.inner
			.cf_handle(column)
			.ok_or_else(|| eyre!("invalid column: {}", column))?;
		let iter = self.inner.prefix_iterator_cf(cf_handle, prefix);
		Ok(Box::new(iter))
	}

	fn read_only(&self) -> bool {
		self.read_only
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

	fn new_dumper<D: IntrospectorKvdb>(input: &D, output_path: &str) -> Result<Self> {
		let mut cf_opts = RocksdbOptions::default();
		let columns = input.list_columns()?.clone();
		cf_opts.create_if_missing(true);
		cf_opts.create_missing_column_families(true);

		let db = DB::open_cf(&cf_opts, output_path, &columns)?;
		Ok(IntrospectorRocksDB { inner: db, columns, read_only: false })
	}
}
