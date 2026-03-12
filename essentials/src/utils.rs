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
use log::{info, warn};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::time::sleep;

#[derive(Clone, Debug, Parser, Default)]
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
	/// Seconds of stable operation after which the retry counter resets.
	/// If the last retry was longer ago than this threshold, the counter
	/// resets to zero so a new series of failures gets the full budget.
	#[clap(default_value = "3600", long)]
	retry_reset_after: u64,
}

pub struct Retry {
	count: u32,
	max_count: u32,
	delay: u32,
	reset_after: Duration,
	last_attempt: Option<Instant>,
}

#[derive(Debug, Error)]
pub enum RetryError {
	#[error("Max count reached")]
	MaxCountReached,
	#[error("Cancelled")]
	Cancelled,
}

impl Default for Retry {
	fn default() -> Self {
		Retry::new(&RetryOptions::default())
	}
}

impl Retry {
	pub fn new(opts: &RetryOptions) -> Self {
		Self {
			count: 0,
			max_count: opts.max_count,
			delay: opts.retry_delay,
			reset_after: Duration::from_secs(opts.retry_reset_after),
			last_attempt: None,
		}
	}

	pub async fn sleep(&mut self) -> color_eyre::Result<(), RetryError> {
		if let Some(last) = self.last_attempt &&
			last.elapsed() >= self.reset_after
		{
			info!("Last retry was over {}s ago, resetting retry counter", self.reset_after.as_secs());
			self.count = 0;
		}
		self.last_attempt = Some(Instant::now());

		self.count += 1;
		if self.count > self.max_count {
			return Err(RetryError::MaxCountReached)
		}

		let exponent = self.count.min(10);
		let ms = (self.delay as u64).saturating_mul(1u64 << exponent);
		warn!("Retrying in {}ms...", ms);
		tokio::select! {
			_ = sleep(Duration::from_millis(ms)) => {},
			_ = tokio::signal::ctrl_c() => return Err(RetryError::Cancelled),
		}

		Ok(())
	}
}
