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

//! Implementation of the introspection using ParityDB

use super::{DBIter, IntrospectorKvdb};
use color_eyre::{eyre::eyre, Result};
use parity_db::{Db, Options as ParityDBOptions};
use std::path::{Path, PathBuf};

pub struct IntrospectorParityDB {
	inner: Db,
	columns: Vec<String>,
	read_only: bool,
	path: PathBuf,
}

impl IntrospectorKvdb for IntrospectorParityDB {
	fn new(path: &std::path::Path) -> Result<Self> {
		let metadata = ParityDBOptions::load_metadata(path)
			.map_err(|e| eyre!("Error resolving metas: {:?}", e))?
			.ok_or_else(|| eyre!("Missing metadata"))?;
		let mut opts = ParityDBOptions::with_columns(path, metadata.columns.len() as u8);
		opts.columns = metadata.columns.clone();
		let db = Db::open_read_only(&opts)?;
		let columns = metadata
			.columns
			.iter()
			.enumerate()
			.map(|(idx, _)| format!("col{}", idx))
			.collect::<Vec<_>>();
		Ok(Self { inner: db, columns, read_only: true, path: path.into() })
	}

	fn list_columns(&self) -> color_eyre::Result<&Vec<String>> {
		Ok(&self.columns)
	}

	fn iter_values(&self, column: &str) -> Result<DBIter> {
		let column_idx = self
			.columns
			.iter()
			.position(|col| col.as_str() == column)
			.ok_or_else(|| eyre!("invalid column: {}", column))? as u8;
		let mut iter = self.inner.iter(column_idx)?;

		Ok(Box::new(std::iter::from_fn(move || {
			if let Some((key, value)) = iter.next().unwrap_or(None) {
				Some((key.into_boxed_slice(), value.into_boxed_slice()))
			} else {
				None
			}
		})))
	}

	fn prefixed_iter_values<'a>(&'a self, column: &str, prefix: &'a str) -> Result<DBIter<'a>> {
		let column_idx = self
			.columns
			.iter()
			.position(|col| col.as_str() == column)
			.ok_or_else(|| eyre!("invalid column: {}", column))? as u8;
		let mut iter = self.inner.iter(column_idx)?;
		iter.seek(prefix.as_bytes())?;

		Ok(Box::new(std::iter::from_fn(move || {
			if let Some((key, value)) = iter.next().unwrap_or(None) {
				key.starts_with(prefix.as_bytes())
					.then(|| (key.into_boxed_slice(), value.into_boxed_slice()))
			} else {
				None
			}
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
		let column_idx = self
			.columns
			.iter()
			.position(|col| col.as_str() == column)
			.ok_or_else(|| eyre!("invalid column: {}", column))? as u8;
		self.inner
			.commit(
				iter.into_iter()
					.map(|(key, value)| (column_idx, key, Some(value.as_ref().to_vec()))),
			)
			.map_err(|e| eyre!("commit error: {:?}", e))
	}

	fn new_dumper<D: IntrospectorKvdb>(input: &D, output_path: &std::path::Path) -> Result<Self> {
		let columns = input.list_columns()?.clone();
		let mut opts = ParityDBOptions::with_columns(output_path, columns.len() as u8);

		// In RocksDB we always have order, and for the ParityDB case it is not always true
		// So we assume that all columns are ordered as a safety measure
		for column in opts.columns.iter_mut() {
			column.btree_index = true;
		}

		let db = Db::open_or_create(&opts)?;
		Ok(IntrospectorParityDB { inner: db, columns, read_only: false, path: output_path.into() })
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;
	use std::path::Path;

	pub fn new_test_parity_db(output_path: &Path, num_columns: usize) -> IntrospectorParityDB {
		let mut opts = ParityDBOptions::with_columns(output_path, num_columns as u8);
		let mut columns: Vec<String> = vec![];

		for (idx, column) in opts.columns.iter_mut().enumerate() {
			column.btree_index = true;
			columns.push(format!("col{}", idx));
		}

		let db = Db::open_or_create(&opts).unwrap();
		IntrospectorParityDB { inner: db, columns, read_only: false, path: output_path.into() }
	}
}
