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

//! This module defines structures used for tool progress tracking

use super::stats::ParachainStats;
use crate::core::api::BlockNumber;
use codec::{Decode, Encode};
use crossterm::style::Stylize;
use std::{
	fmt::{self, Display},
	time::Duration,
};
use subxt::sp_core::H256;

#[derive(Clone, Default)]
pub struct BitfieldsHealth {
	/// Maximum bitfield count, equal to number of parachain validators.
	pub max_bitfield_count: u32,
	/// Current bitfield count in the relay chain block.
	pub bitfield_count: u32,
	/// Sum of all bits for a given parachain.
	pub available_count: u32,
}

#[derive(Clone)]
/// Events related to parachain blocks from consensus perspective.
pub enum ParachainConsensusEvent {
	/// A core has been assigned to a parachain.
	CoreAssigned(u32),
	/// A candidate was backed.
	Backed(H256),
	/// A candidate was included.
	Included(H256),
	/// A dispute has concluded.
	Disputed(DisputesOutcome),
	/// No candidate backed.
	SkippedSlot,
	/// Candidate not available yet.
	SlowAvailability,
	/// Inherent bitfield count is lower than 2/3 of expect.
	SlowBitfieldPropagation,
}

#[derive(Clone, Default)]
/// Contains information about how a parachain has progressed at a given relay
/// chain block.
pub struct ParachainProgressUpdate {
	/// Parachain id.
	pub para_id: u32,
	/// Block timestamp.
	pub timestamp: u64,
	/// Previous timestamp
	pub prev_timestamp: u64,
	/// Relay chain block number.
	pub block_number: BlockNumber,
	/// Relay chain block hash.
	pub block_hash: H256,
	/// Bitfields health metrics.
	pub bitfield_health: BitfieldsHealth,
	/// Core occupation.
	pub core_occupied: bool,
	/// Consensus events happening for the para under a relay parent.
	pub events: Vec<ParachainConsensusEvent>,
}

#[derive(Encode, Decode, Debug, Default, Clone)]
pub struct DisputesOutcome {
	pub candidate: H256,
	pub voted_for: u32,
	pub voted_against: u32,
	pub misbehaving_validators: Vec<(u32, String)>,
}

/// Format the current block inherent timestamp.
fn format_ts(duration: Duration, current_block_ts: u64) -> String {
	let dt = time::OffsetDateTime::from_unix_timestamp_nanos(current_block_ts as i128 * 1_000_000).unwrap();
	format!(
		"{} +{}",
		dt.format(&time::format_description::well_known::Iso8601::DEFAULT)
			.expect("Invalid datetime format"),
		format_args!("{}ms", duration.as_millis())
	)
}

impl Display for ParachainProgressUpdate {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"{} [#{}] ",
			format_ts(Duration::from_millis(self.timestamp.saturating_sub(self.prev_timestamp)), self.timestamp),
			self.block_number
		)?;
		for event in &self.events {
			event.fmt(f)?;
		}
		writeln!(f, "\t🔗 Relay block hash: {} ", format!("{:?}", self.block_hash).bold())?;
		writeln!(f, "\t🥝 Availability core {}", if !self.core_occupied { "FREE" } else { "OCCUPIED" })
	}
}

impl Display for ParachainConsensusEvent {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		writeln!(f, "nyi!")
	}
}
