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

mod rocksdb;
mod traits;

use clap::Parser;
use color_eyre::Result;
use serde::Serialize;
use std::fmt::{Display, Formatter};
use strum::Display;
use strum::EnumString;

pub use crate::kvdb::traits::*;

/// Mode of this command
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum KvdbMode {
	/// Returns list of all columns in the database
	Columns,
	/// Returns usage in the database
	Usage,
}

/// Mode of this command
#[derive(Clone, Debug, Parser, EnumString, Display)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum KvdbType {
	/// RocksDB database
	RocksDB,
	/// ParityDB database
	ParityDB,
}

impl Default for KvdbType {
	fn default() -> Self {
		KvdbType::RocksDB
	}
}

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub struct KvdbOptions {
	/// Path to the database
	#[clap(long)]
	db: String,

	#[clap(long, default_value_t)]
	db_type: KvdbType,
	/// Mode of running
	#[clap(subcommand)]
	mode: KvdbMode,
}

#[derive(Clone, Debug, Serialize)]
struct UsageResults {
	keys_count: usize,
	keys_size: usize,
	values_size: usize,
}

impl Display for UsageResults {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		if self.keys_count > 0 {
			write!(
				f,
				"{} keys size: {} bytes, values size: {} bytes ({:.2} bytes per key in average)",
				self.keys_count,
				self.keys_size,
				self.values_size,
				self.values_size as f64 / self.keys_count as f64
			)
		} else {
			write!(
				f,
				"{} keys size: {} bytes, values size: {} bytes (0 bytes per key in average)",
				self.keys_count, self.keys_size, self.values_size
			)
		}
	}
}

pub fn inrospect_kvdb(opts: KvdbOptions) -> Result<()> {
	match opts.db_type {
		KvdbType::RocksDB => run_with_db(rocksdb::IntrospectorRocksDB::new(opts.db.as_str())?, opts),
		KvdbType::ParityDB => {
			todo!();
		},
	}
}

fn run_with_db<D: IntrospectorKvdb>(db: D, opts: KvdbOptions) -> Result<()> {
	match opts.mode {
		KvdbMode::Columns => {
			let columns = db.list_columns()?;

			for col in columns {
				println!("{}", col.as_str());
			}
		},
		KvdbMode::Usage => {
			let columns = db.list_columns()?;

			for col in columns {
				let mut keys_space = 0_usize;
				let mut keys_count = 0_usize;
				let mut values_space = 0_usize;
				let iter = db.iter_values(col.as_str())?;
				for (key, value) in iter {
					keys_space += key.len();
					keys_count += 1;
					values_space += value.len();
				}

				let res = UsageResults { keys_count, values_size: values_space, keys_size: keys_space };

				println!("{}: {}", col.as_str(), res);
			}
		},
	}

	Ok(())
}
