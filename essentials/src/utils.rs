// Copyright 2023 Parity Technologies (UK) Ltd.
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
//

use clap::Parser;
use log::info;
use std::time::Duration;
use thiserror::Error;
use tokio::time::sleep;

#[derive(Clone, Debug, Parser)]
#[clap(
	rename_all = "kebab-case",
	allow_external_subcommands = true, // HACK: to parse it as a standalone config
)]
pub struct RetryOptions {
	/// Max of times to retry a failed connection before giving up.
	#[clap(name = "retry", default_value = "10", long)]
	max_count: u32,
	/// Delay in ms to wait between retry attempts
	#[clap(default_value = "100", long)]
	retry_delay: u32,
}

pub struct Retry {
	count: u32,
	max_count: u32,
	delay: u32,
}

#[derive(Debug, Error)]
pub enum RetryError {
	#[error("Max count reached")]
	MaxCountReached,
}

impl Default for Retry {
	fn default() -> Self {
		Self::new()
	}
}

impl Retry {
	pub fn new() -> Self {
		let opts = RetryOptions::parse();

		Self { count: 0, max_count: opts.max_count, delay: opts.retry_delay }
	}

	pub async fn sleep(&mut self) -> color_eyre::Result<(), RetryError> {
		self.count += 1;
		if self.count > self.max_count {
			return Err(RetryError::MaxCountReached)
		}

		let ms = self.delay * (self.count + 1);
		info!("Retrying in {}ms...", ms);
		sleep(Duration::from_millis(ms.into())).await;

		Ok(())
	}
}
