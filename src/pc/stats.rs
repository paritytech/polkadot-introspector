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

use super::tracker::DisputesTracker;
use color_eyre::owo_colors::OwoColorize;
use crossterm::style::Stylize;
use std::{
	default::Default,
	fmt::{self, Display, Formatter},
	time::Duration,
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
	/// Average time (calculated using `CMA`: cumulative moving average).
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
	/// Update counter value
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

	/// Returns counter value
	pub fn value(&self) -> f64 {
		if self.num_samples > 0 {
			self.avg
		} else {
			f64::NAN
		}
	}

	/// Number of samples
	pub fn count(&self) -> usize {
		self.num_samples
	}
}

/// Tracker of the disputes
#[derive(Clone, Default)]
struct DisputesStats {
	/// Number of candidates disputed.
	disputed_count: u32,
	concluded_valid: u32,
	concluded_invalid: u32,
	/// Average count of validators that voted against supermajority
	misbehaving_validators: AvgBucket<u32>,
	/// Average resolution time in blocks
	resolution_time: AvgBucket<u32>,
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
	skipped_slots: Vec<u32>,
	/// Number of candidates included.
	included_count: u32,
	/// Disputes stats
	disputes_stats: DisputesStats,
	/// Block time measurements for relay parent blocks
	block_times: AvgBucket<f32>,
	/// Number of slow availability events.
	slow_avail_count: u32,
	/// Number of low bitfield propagation events.
	low_bitfields_count: u32,
	/// Number of bitfields being set
	bitfields: AvgBucket<u32>,
	/// Average included time in relay parent blocks
	included_times: AvgBucket<u16>,
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
	pub fn on_included(&mut self, relay_parent_number: u32, previous_included: Option<u32>) {
		self.included_count += 1;

		if let Some(previous_block_number) = previous_included {
			self.included_times
				.update(relay_parent_number.saturating_sub(previous_block_number) as u16);
		}
	}

	/// Update disputed counter
	pub fn on_disputed(&mut self, dispute_outcome: &DisputesTracker) {
		self.disputes_stats.disputed_count += 1;

		if dispute_outcome.voted_for > dispute_outcome.voted_against {
			self.disputes_stats.concluded_valid += 1;
		} else {
			self.disputes_stats.concluded_invalid += 1;
		}

		self.disputes_stats
			.misbehaving_validators
			.update(dispute_outcome.misbehaving_validators.len() as u32);

		if let Some(diff) = dispute_outcome.resolve_time {
			self.disputes_stats.resolution_time.update(diff);
		}
	}

	/// Track block
	pub fn on_block(&mut self, time: Duration) {
		self.block_times.update(time.as_secs_f32());
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
	pub fn on_skipped_slot(&mut self, block_number: u32) {
		self.skipped_slots.push(block_number);
	}
}

impl Display for ParachainStats {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		writeln!(f, "{}", format!("--- Parachain {} trace statistics ---", self.para_id).bold().blue())?;
		writeln!(
			f,
			"Average relay chain block time: {} seconds ({} blocks processed)",
			format!("{:.3}", self.block_times.value()).bold(),
			self.block_times.count()
		)?;
		writeln!(
			f,
			"Average parachain block time: {} relay parent blocks ({} parachain blocks processed)",
			format!("{:.2}", self.included_times.value()).bold(),
			self.included_times.count()
		)?;
		writeln!(
			f,
			"Skipped slots: {}, {:?}",
			self.skipped_slots.len().to_string().bright_purple(),
			self.skipped_slots,
		)?;
		writeln!(
			f,
			"Slow availability: {}, slow bitfields propagation: {}",
			self.slow_avail_count.to_string().bright_cyan(),
			self.low_bitfields_count.to_string().bright_magenta()
		)?;
		writeln!(f, "Average bitfileds: {:.3}", self.bitfields.value())?;
		writeln!(
			f,
			"Backing stats: {} blocks backed, {} blocks included",
			self.backed_count.to_string().blue(),
			self.included_count.to_string().green()
		)?;
		writeln!(f, "Disputes stats: {}", self.disputes_stats)
	}
}

impl Display for DisputesStats {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"{} disputes tracked, {} concluded valid, {} concluded invalid, {} blocks average resolution time, {} average misbehaving validators",
			self.disputed_count,
			self.concluded_valid.to_string().bright_green(),
			self.concluded_invalid.to_string().bold(),
			format!("{:.2}", self.resolution_time.value()).bold(),
			format!("{:.1}", self.misbehaving_validators.value()).bright_red()
		)
	}
}
