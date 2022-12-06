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

//! This module keep tracks of the statistics for the parachain events

use crossterm::style::Stylize;
use std::time::Duration;
use std::{
	fmt,
	fmt::{Debug, Display},
};

trait UsableNumber: PartialOrd + PartialEq + Into<f64> + Copy {
	fn max() -> Self;
	fn min() -> Self;
}

macro_rules! usable_number {
	($ty: ident) => {
		impl UsableNumber for $ty {
			fn max() -> Self {
				$ty::MAX
			}

			fn min() -> Self {
				$ty::MIN
			}
		}
	};
}
usable_number!(u32);
usable_number!(u16);
usable_number!(u8);
usable_number!(f32);

#[derive(Clone)]
/// Parachain block time stats.
struct AvgBucket<T: UsableNumber> {
	/// Average time (calculated using CMA).
	pub avg: f64,
	/// Max time.
	pub max: T,
	/// Min time.
	pub min: T,
	/// Number of samples
	pub num_samples: usize,
}

impl<T: UsableNumber> Default for AvgBucket<T> {
	fn default() -> Self {
		Self { avg: 0.0, max: T::min(), min: T::max(), num_samples: 0 }
	}
}

impl<T: UsableNumber> AvgBucket<T> {
	pub fn update(&mut self, new_value: T) {
		if self.max < new_value {
			self.max = new_value;
		}
		if self.min > new_value {
			self.min = new_value;
		}
		self.num_samples += 1;
		let new_value_float: f64 = new_value.into();
		self.avg += (new_value_float - self.avg) / self.num_samples as f64;
	}
}

#[derive(Clone, Default)]
/// Per parachain statistics
pub struct ParachainStats {
	/// Parachain id.
	para_id: u32,
	/// Number of backed candidates.
	backed_count: u32,
	/// Number of skipped slots, where no candidate was backed and availability core
	/// was free.
	skipped_slots: u32,
	/// Number of candidates included.
	included_count: u32,
	/// Number of candidates disputed.
	disputed_count: u32,
	/// Block time measurements.
	block_times: AvgBucket<f32>,
	/// Number of slow availability events.
	slow_avail_count: u32,
	/// Number of low bitfield propagation events.
	low_bitfields_count: u32,
	/// Number of bitfields being set
	bitfields: AvgBucket<u32>,
}

impl ParachainStats {
	/// Returns a new tracker
	pub fn new(para_id: u32) -> Self {
		Self { para_id, ..Default::default() }
	}

	/// Update backed counter
	pub fn on_backed(&mut self) {
		self.backed_count += 1;
	}

	/// Update included counter
	pub fn on_included(&mut self) {
		self.included_count += 1;
	}

	/// Update disputed counter
	pub fn on_disputed(&mut self) {
		self.disputed_count += 1;
	}

	/// Track block
	pub fn on_block(&mut self, time: Duration) {
		self.block_times.update(time.as_secs_f32().into());
	}

	/// Track bitfields
	pub fn on_bitfields(&mut self, nbits: u32, is_low: bool) {
		self.bitfields.update(nbits);
		if is_low {
			self.low_bitfields_count += 1;
		}
	}

	/// Notice slow availability
	pub fn on_slow_availability(&mut self) {
		self.slow_avail_count += 1;
	}

	/// Update skipped slots count
	pub fn on_skipped_slot(&mut self) {
		self.skipped_slots += 1;
	}
}

impl Display for ParachainStats {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		writeln!(
			f,
			"{}",
			format!("--- Parachain {} trace statistics ---", self.para_id)
				.to_string()
				.bold()
				.blue()
		)
	}
}
