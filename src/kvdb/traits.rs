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

use color_eyre::Result;

pub type DBIter<'a> = Box<dyn Iterator<Item = (Box<[u8]>, Box<[u8]>)> + 'a>;
/// A minimum subset of the functions required to open a database for introspection
pub trait IntrospectorKvdb {
	/// Opens database with some configuration
	fn new(path: &std::path::Path) -> Result<Self>
	where
		Self: Sized;
	/// List all column families in a database
	fn list_columns(&self) -> Result<&Vec<String>>;
	/// Iterates over all keys in a specific column
	fn iter_values<'a>(&'a self, column: &str) -> Result<DBIter<'a>>;
	/// Iterates over all keys that begin with the specific prefix, column must have order defined
	fn prefixed_iter_values<'a>(&'a self, column: &str, prefix: &'a str) -> Result<DBIter<'a>>;
	/// Returns if kvdb is in read-only mode
	fn read_only(&self) -> bool {
		true
	}
	/// Returns a path to database
	fn get_db_path(&self) -> &std::path::Path;
	/// Writes an iterator of columns/keys/values to the kvdb  (kvdb must be not in the read-only mode)
	fn write_iter<I, K, V>(&self, column: &str, iter: I) -> Result<()>
	where
		I: IntoIterator<Item = (K, V)>,
		K: AsRef<[u8]>,
		V: AsRef<[u8]>;
	/// Create a database dump engine
	fn new_dumper<D: IntrospectorKvdb>(input: &D, output_path: &std::path::Path) -> Result<Self>
	where
		Self: Sized;
}
