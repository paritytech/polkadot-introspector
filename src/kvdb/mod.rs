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

mod decode;
mod paritydb;
mod rocksdb;
mod traits;

use crate::kvdb::decode::DecodedOutput;
use clap::Parser;
use color_eyre::Result;
use serde::Serialize;
use std::fmt::{Display, Formatter};
use strum::{Display, EnumString};

pub use crate::kvdb::traits::*;

/// Specific options for the usage subcommand
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct KvdbUsageOpts {
	/// Check only specific column(s)
	#[clap(long, short = 'c')]
	column: Vec<String>,
	/// Limit scan by specific key prefix(es)
	#[clap(long, short = 'p')]
	keys_prefix: Vec<String>,
}

/// Specific options for the keys subcommand
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct KvdbKeysOpts {
	/// Check only specific column(s)
	#[clap(long, short = 'c')]
	column: String,
	/// Decode keys matching the specific format (like `candidate-votes%i%h`, where `%i` represents a big-endian integer)
	#[clap(long, short = 'f')]
	fmt: String,
	/// Limit number of output entries
	#[clap(long, short = 'l')]
	limit: Option<usize>,
}

/// Mode of this command
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum KvdbMode {
	/// Returns list of all columns in the database
	Columns,
	/// Returns usage in the database
	Usage(KvdbUsageOpts),
	/// Decode specific keys in the database
	DecodeKeys(KvdbKeysOpts),
}

/// Database type
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

/// Output mode for the CLI commands
#[derive(Clone, Debug, Parser, EnumString, Display)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum OutputMode {
	#[strum(ascii_case_insensitive)]
	Pretty,
	#[strum(ascii_case_insensitive)]
	Json,
}

impl Default for OutputMode {
	fn default() -> Self {
		OutputMode::Pretty
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
	/// Output mode
	#[clap(long, default_value_t)]
	output: OutputMode,
}

#[derive(Clone, Debug, Serialize)]
struct UsageResults<'a> {
	description: &'a str,
	keys_count: usize,
	keys_size: usize,
	values_size: usize,
}

impl<'a> Display for UsageResults<'a> {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		if self.keys_count > 0 {
			write!(
				f,
				"{}: {} keys size: {} bytes ({:.2} bytes per key in average), values size: {} bytes ({:.2} bytes per value in average)",
				self.description,
				self.keys_count,
				self.keys_size,
				self.keys_size as f64 / self.keys_count as f64,
				self.values_size,
				self.values_size as f64 / self.keys_count as f64
			)
		} else {
			write!(
				f,
				"{}: {} keys size: {} bytes (0 bytes per key in average), values size: {} bytes (0 bytes per value in average)",
				self.description, self.keys_count, self.keys_size, self.values_size
			)
		}
	}
}

pub fn introspect_kvdb(opts: KvdbOptions) -> Result<()> {
	match opts.db_type {
		KvdbType::RocksDB => run_with_db(rocksdb::IntrospectorRocksDB::new(opts.db.as_str())?, opts),
		KvdbType::ParityDB => run_with_db(paritydb::IntrospectorParityDB::new(opts.db.as_str())?, opts),
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
		KvdbMode::Usage(ref usage_opts) => {
			let columns = db.list_columns()?.iter().filter(|col| {
				if !usage_opts.column.is_empty() {
					usage_opts.column.contains(col)
				} else {
					true
				}
			});

			for col in columns {
				let mut keys_space = 0_usize;
				let mut keys_count = 0_usize;
				let mut values_space = 0_usize;

				if usage_opts.keys_prefix.is_empty() {
					let iter = db.iter_values(col.as_str())?;
					for (key, value) in iter {
						keys_space += key.len();
						keys_count += 1;
						values_space += value.len();
					}
				} else {
					// Iterate over all requested prefixes
					for prefix in &usage_opts.keys_prefix {
						let iter = db.prefixed_iter_values(col.as_str(), prefix.as_str())?;
						for (key, value) in iter {
							keys_space += key.len();
							keys_count += 1;
							values_space += value.len();
						}
					}
				}

				let res = UsageResults {
					description: col.as_str(),
					keys_count,
					values_size: values_space,
					keys_size: keys_space,
				};

				output_result(&res, &opts)?;
			}
		},
		KvdbMode::Keys(ref kvdb_keys_opts) => {
			let res = decode::decode_keys(
				&db,
				kvdb_keys_opts.column.as_str(),
				kvdb_keys_opts.fmt.as_str(),
				&kvdb_keys_opts.limit,
			)?;
			output_decoded_keys(&res, &opts)?;
		},
	}

	Ok(())
}

fn output_result<T>(res: &T, opts: &KvdbOptions) -> Result<()>
where
	T: Display + Serialize,
{
	match opts.output {
		OutputMode::Json => println!("{}", serde_json::to_string(res)?),
		OutputMode::Pretty => println!("{}", res),
	}

	Ok(())
}

fn output_decoded_keys(res: &DecodedOutput, opts: &KvdbOptions) -> Result<()> {
	match opts.output {
		OutputMode::Json => {
			println!("{}", serde_json::to_string(res)?);
		},
		OutputMode::Pretty =>
			for elt in res {
				println!("{:?}", elt);
			},
	}

	Ok(())
}
