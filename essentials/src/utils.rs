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

use std::time::Duration;

use log::info;
use thiserror::Error;
use tokio::time::sleep;

use crate::constants::{RETRY_COUNT, RETRY_DELAY_MS};

#[derive(Default)]
pub struct Retry {
	count: u64,
}

#[derive(Debug, Error)]
pub enum RetryError {
	#[error("Max count reached")]
	MaxCountReached,
}

impl Retry {
	pub async fn sleep(&mut self) -> color_eyre::Result<(), RetryError> {
		self.count += 1;
		if self.count > RETRY_COUNT {
			return Err(RetryError::MaxCountReached)
		}

		let ms = RETRY_DELAY_MS * (self.count + 1);
		info!("Retrying in {}ms...", ms);
		sleep(Duration::from_millis(ms)).await;

		Ok(())
	}
}
