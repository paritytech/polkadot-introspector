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

use super::IntrospectorKvdb;
use color_eyre::Result;
use rocksdb::{Options as RocksdbOptions, DB};

pub struct IntrospectorRocksDB {
	inner: DB,
	columns: Vec<String>,
}

impl IntrospectorKvdb for IntrospectorRocksDB {
	fn new(path: &str) -> Result<Self> {
		let cf_opts = RocksdbOptions::default();
		let columns = DB::list_cf(&cf_opts, path)?;
		let db = DB::open_for_read_only(&cf_opts, path, false)?;
		Ok(Self { inner: db, columns })
	}

	fn list_columns(&self) -> color_eyre::Result<&Vec<String>> {
		Ok(&self.columns)
	}
}
