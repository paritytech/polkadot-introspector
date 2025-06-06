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

use crate::types::{DisputesTracker, ParachainProgressUpdate};
use color_eyre::owo_colors::OwoColorize;
use crossterm::style::Stylize;
use mockall::automock;
use polkadot_introspector_essentials::types::H256;
use std::{
	collections::VecDeque,
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
		if self.num_samples > 0 { self.avg } else { f64::NAN }
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

#[derive(Clone)]
struct SkippedSlotBlock {
	block_number: u32,
	block_hash: H256,
}

impl SkippedSlotBlock {
	pub fn new(update: &ParachainProgressUpdate) -> Self {
		Self { block_number: update.block_number, block_hash: update.block_hash }
	}
}

impl Display for SkippedSlotBlock {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "\n    {} {:?}", self.block_number, self.block_hash)
	}
}

fn join_skipped_slot_blocks_to_string(blocks: &VecDeque<SkippedSlotBlock>) -> String {
	if blocks.is_empty() { String::from("none") } else { blocks.iter().map(|b| b.to_string()).collect() }
}

#[automock]
pub trait Stats {
	fn on_backed(&mut self);
	fn on_included(&mut self, relay_parent_number: u32, previous_included: Option<u32>, backed_in: Option<u32>);
	fn on_disputed(&mut self, dispute_outcome: &DisputesTracker);
	fn on_block(&mut self, time: Duration);
	fn on_bitfields(&mut self, nbits: u32, is_low: bool);
	fn on_slow_availability(&mut self);
	fn on_skipped_slot(&mut self, update: &ParachainProgressUpdate);
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
	/// Last relay blocks for skipped slots
	last_skipped_slot_blocks: VecDeque<SkippedSlotBlock>,
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
	/// Average backing time in relay parent blocks
	backed_times: AvgBucket<u16>,
}

impl ParachainStats {
	/// Returns a new tracker
	///
	/// # Arguments
	///
	/// * `para_id` - Parachain id
	/// * `last_skipped_slot_blocks` - The number of last blocks with missing slots
	pub fn new(para_id: u32, last_skipped_slot_blocks: usize) -> Self {
		Self {
			para_id,
			last_skipped_slot_blocks: VecDeque::with_capacity(last_skipped_slot_blocks),
			..Default::default()
		}
	}
}
impl Stats for ParachainStats {
	/// Update backed counter
	fn on_backed(&mut self) {
		self.backed_count += 1;
	}

	/// Update included counter
	fn on_included(&mut self, relay_parent_number: u32, previous_included: Option<u32>, backed_in: Option<u32>) {
		self.included_count += 1;

		if let Some(previous_block_number) = previous_included {
			self.included_times
				.update(relay_parent_number.saturating_sub(previous_block_number) as u16);
		}

		if let Some(backed_in) = backed_in {
			self.backed_times.update(backed_in as u16);
		}
	}

	/// Update disputed counter
	fn on_disputed(&mut self, dispute_outcome: &DisputesTracker) {
		self.disputes_stats.disputed_count += 1;

		if dispute_outcome.voted_for > dispute_outcome.voted_against {
			self.disputes_stats.concluded_valid += 1;
		} else {
			self.disputes_stats.concluded_invalid += 1;
		}

		self.disputes_stats
			.misbehaving_validators
			.update(dispute_outcome.misbehaving_validators.len() as u32);
		self.disputes_stats.resolution_time.update(dispute_outcome.resolve_time);
	}

	/// Track block
	fn on_block(&mut self, time: Duration) {
		self.block_times.update(time.as_secs_f32());
	}

	/// Track bitfields
	fn on_bitfields(&mut self, nbits: u32, is_low: bool) {
		self.bitfields.update(nbits);
		if is_low {
			self.low_bitfields_count += 1;
		}
	}

	/// Notice slow availability
	fn on_slow_availability(&mut self) {
		self.slow_avail_count += 1;
	}

	/// Update count and last blocks details for skipped slots
	fn on_skipped_slot(&mut self, update: &ParachainProgressUpdate) {
		self.skipped_slots += 1;

		if self.last_skipped_slot_blocks.len() >= self.last_skipped_slot_blocks.capacity() {
			self.last_skipped_slot_blocks.pop_front();
		}
		self.last_skipped_slot_blocks.push_back(SkippedSlotBlock::new(update))
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
			"Average parachain block inclusion time: {} relay parent blocks ({} parachain blocks processed)",
			format!("{:.2}", self.included_times.value()).bold(),
			self.included_times.count()
		)?;
		writeln!(
			f,
			"Average parachain block backing time: {} relay parent blocks ({} parachain blocks processed)",
			format!("{:.2}", self.backed_times.value()).bold(),
			self.backed_times.count()
		)?;
		writeln!(
			f,
			"Skipped slots: {}, slow availability: {}, slow bitfields propagation: {}",
			self.skipped_slots.to_string().bright_purple(),
			self.slow_avail_count.to_string().bright_cyan(),
			self.low_bitfields_count.to_string().bright_magenta()
		)?;
		writeln!(
			f,
			"Last blocks with skipped slots: {}",
			join_skipped_slot_blocks_to_string(&self.last_skipped_slot_blocks).bright_purple()
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
