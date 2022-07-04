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
use std::cell::RefCell;
use std::rc::Rc;
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

pub fn inrospect_kvdb(opts: KvdbOptions) -> Result<()> {
	let db: Box<dyn traits::IntrospectorKvdb> = match opts.db_type {
		KvdbType::RocksDB => Box::new(rocksdb::IntrospectorRocksDB::new(opts.db.as_str())?),
		KvdbType::ParityDB => {
			todo!();
		},
	};

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
				let keys_space = Rc::new(RefCell::new(0_usize));
				let keys_count = Rc::new(RefCell::new(0_usize));
				let values_space = Rc::new(RefCell::new(0_usize));

				db.iter_values(col.as_str(), &|key: &[u8], value: &[u8]| {
					*keys_space.borrow_mut() += key.len();
					*keys_count.borrow_mut() += 1;
					*values_space.borrow_mut() += value.len();
					true // Continue iterations
				})
				.ok();

				println!(
					"{}: {} keys size: {} bytes, values size: {} bytes",
					col.as_str(),
					keys_count.borrow(),
					keys_space.borrow(),
					values_space.borrow()
				);
			}
		},
	}

	Ok(())
}
