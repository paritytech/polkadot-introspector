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
//! This module tracks parachain blocks.
use crate::core::api::BlockNumber;
use codec::{Decode, Encode};
use crossterm::style::Stylize;
use std::{
	fmt,
	fmt::{Debug, Display},
};
use subxt::sp_core::H256;

#[derive(Default, Clone)]
/// Parachain block time stats.
pub struct ParaBlockTime {
	/// Average block time.
	pub avg: u16,
	/// Max block time.
	pub max: u16,
	/// Min block time.
	pub min: u16,
}

#[derive(Clone, Default)]
/// Per parachain statistics
pub struct ParachainStats {
	/// Parachain id.
	pub para_id: u32,
	/// Number of backed candidates.
	pub backed_count: u32,
	/// Number of skipped slots, where no candidate was backed and availability core
	/// was free.
	pub skipped_slots: u32,
	/// Number of candidates included.
	pub included_count: u32,
	/// Number of candidates disputed.
	pub disputed_count: u32,
	/// Block time measurements.
	pub block_times: ParaBlockTime,
	/// Number of slow availability events.
	pub slow_avail_count: u32,
	/// Number of low bitfield propagation events.
	pub slow_bitfields_count: u32,
}

// We need something similar to this:
//
// PING google.com (142.250.185.174) 56(84) bytes of data.
// 64 bytes from fra16s51-in-f14.1e100.net (142.250.185.174): icmp_seq=1 ttl=60 time=10.3 ms
// 64 bytes from fra16s51-in-f14.1e100.net (142.250.185.174): icmp_seq=2 ttl=60 time=10.4 ms
// 64 bytes from fra16s51-in-f14.1e100.net (142.250.185.174): icmp_seq=3 ttl=60 time=10.3 ms
// ^C
// --- google.com ping statistics ---
// 3 packets transmitted, 3 received, 0% packet loss, time 2002ms
// rtt min/avg/max/mdev = 10.297/10.323/10.362/0.027 ms

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

impl Display for ParachainProgressUpdate {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		writeln!(
			f,
			"{}",
			format!("--- Parachain {} progress update ---", self.para_id)
				.to_string()
				.bold()
				.blue()
		)
	}
}

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
	/// The core is now occupied.
	CoreOccupied,
	/// The core is now free.
	CoreFree,
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
	/// Relay chain block number.
	pub block_number: BlockNumber,
	/// Relay chain block hash.
	pub block_hash: H256,
	/// Bitfields health metrics.
	pub bitfield_health: BitfieldsHealth,
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
