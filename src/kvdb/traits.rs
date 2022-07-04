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

/// A minimum subset of the functions required to open a database for introspection
pub trait IntrospectorKvdb {
	/// Opens database with some configuration
	fn new(path: &str) -> Result<Self>
	where
		Self: Sized;
	/// List all column families in a database
	fn list_columns(&self) -> Result<&Vec<String>>;
	/// Iterates over all keys in a specific column, calling a specific functor
	fn iter_values(&self, column: &str, func: &dyn Fn(&[u8], &[u8]) -> bool) -> Result<bool>;
}
