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

use crate::disputes_tracker::DisputesTracker;
use color_eyre::owo_colors::OwoColorize;
use crossterm::style::Stylize;
use polkadot_introspector_essentials::{
	api::subxt_wrapper::SubxtHrmpChannel,
	chain_events::SubxtDisputeResult,
	types::{BlockNumber, Timestamp, H256},
};
use std::{
	fmt::{self, Display, Formatter, Write},
	time::Duration,
};

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
	/// A candidate was included, including availability bits
	Included(H256, u32, u32),
	/// A dispute has concluded.
	Disputed(DisputesTracker),
	/// No candidate backed.
	SkippedSlot,
	/// Candidate not available yet, including availability bits
	SlowAvailability(u32, u32),
	/// Inherent bitfield count is lower than 2/3 of expect.
	SlowBitfieldPropagation(u32, u32),
	/// New session occurred
	NewSession(u32),
	/// HRMP messages queues for inbound and outbound transfers
	MessageQueues(Vec<(u32, SubxtHrmpChannel)>, Vec<(u32, SubxtHrmpChannel)>),
}

#[derive(Clone, Default)]
/// Contains information about how a parachain has progressed at a given relay
/// chain block.
pub struct ParachainProgressUpdate {
	/// Parachain id.
	pub para_id: u32,
	/// Block timestamp.
	pub timestamp: Timestamp,
	/// Previous timestamp
	pub prev_timestamp: Timestamp,
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
	/// If we are in the fork chain, then this flag will be `true`
	pub is_fork: bool,
	/// Finality lag (best block number - last finalized block number)
	pub finality_lag: Option<u32>,
}

impl ParachainProgressUpdate {
	pub fn new(
		para_id: u32,
		timestamp: Option<Timestamp>,
		prev_timestamp: Option<Timestamp>,
		block_number: BlockNumber,
		block_hash: H256,
		is_fork: bool,
		finality_lag: Option<u32>,
	) -> Self {
		let timestamp = timestamp.unwrap_or_default();
		let prev_timestamp = prev_timestamp.unwrap_or(timestamp);
		Self {
			para_id,
			timestamp,
			prev_timestamp,
			block_number,
			block_hash,
			is_fork,
			finality_lag,
			..Default::default()
		}
	}
}

/// Format the current block inherent timestamp.
fn format_ts(duration: Duration, current_block_ts: Timestamp) -> String {
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
		let mut buf = String::with_capacity(8192);
		if self.is_fork {
			writeln!(
				buf,
				"{} [#{}, fork]; parachain: {}",
				format_ts(Duration::from_millis(self.timestamp.saturating_sub(self.prev_timestamp)), self.timestamp),
				self.block_number,
				format!("{}", self.para_id).bold(),
			)?;
		} else {
			writeln!(
				buf,
				"{} [#{}]; parachain: {}",
				format_ts(Duration::from_millis(self.timestamp.saturating_sub(self.prev_timestamp)), self.timestamp),
				self.block_number,
				format!("{}", self.para_id).bold(),
			)?;
		}
		for event in &self.events {
			write!(buf, "{}", event)?;
		}
		writeln!(buf, "\t🔗 Relay block hash: {} ", format!("{:?}", self.block_hash).bold())?;
		writeln!(buf, "\t🥝 Availability core {}", if !self.core_occupied { "FREE" } else { "OCCUPIED" })?;
		writeln!(
			buf,
			"\t🐌 Finality lag: {}",
			self.finality_lag
				.map_or_else(|| "NA".to_owned(), |lag| format!("{} blocks", lag))
		)?;
		f.write_str(buf.as_str())
	}
}

impl Display for ParachainConsensusEvent {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			ParachainConsensusEvent::CoreAssigned(core_id) => {
				writeln!(f, "\t- Parachain assigned to core index {}", core_id)
			},
			ParachainConsensusEvent::Backed(candidate_hash) => {
				writeln!(f, "\t{}", "CANDIDATE BACKED".to_string().bold().green())?;
				writeln!(f, "\t💜 Candidate hash: {} ", format!("{:?}", candidate_hash).magenta())
			},
			ParachainConsensusEvent::Included(candidate_hash, bits_available, max_bits) => {
				writeln!(f, "\t{}", "CANDIDATE INCLUDED".to_string().bold().green())?;
				writeln!(f, "\t💜 Candidate hash: {} ", format!("{:?}", candidate_hash).magenta())?;
				writeln!(f, "\t🟢 Availability bits: {}/{}", bits_available, max_bits)
			},
			ParachainConsensusEvent::Disputed(outcome) => {
				writeln!(f, "\t{}", "💔 Dispute tracked:".to_string().bold())?;
				write!(f, "{}", outcome)
			},
			ParachainConsensusEvent::SkippedSlot => {
				writeln!(f, "\t{}, no candidate backed", "SLOW BACKING".to_string().bold().red(),)
			},
			ParachainConsensusEvent::SlowAvailability(bits_available, max_bits) => {
				writeln!(f, "\t{}", "SLOW AVAILABILITY".to_string().bold().yellow())?;
				writeln!(f, "\t🟢 Availability bits: {}/{}", bits_available, max_bits)
			},
			ParachainConsensusEvent::SlowBitfieldPropagation(bitfields_count, max_bits) => {
				writeln!(
					f,
					"\t{} bitfield count {}/{}",
					"SLOW BITFIELD PROPAGATION".to_string().dark_red(),
					bitfields_count,
					max_bits
				)
			},
			ParachainConsensusEvent::NewSession(session_index) => {
				writeln!(f, "\t✨ New session tracked: {}", session_index)
			},
			ParachainConsensusEvent::MessageQueues(inbound, outbound) => {
				if !inbound.is_empty() {
					let total: u32 = inbound.iter().map(|(_, channel)| channel.total_size).sum();
					writeln!(f, "\t👉 Inbound HRMP messages, received {} bytes in total", total)?;

					for (peer_parachain, channel) in inbound {
						if channel.total_size > 0 {
							writeln!(
								f,
								"\t\t📩 From parachain: {}, {} bytes / {} max",
								peer_parachain, channel.total_size, channel.max_message_size
							)?;
						}
					}
				}
				if !outbound.is_empty() {
					let total: u32 = inbound.iter().map(|(_, channel)| channel.total_size).sum();
					writeln!(f, "\t👉 Outbound HRMP messages, sent {} bytes in total", total)?;

					for (peer_parachain, channel) in outbound {
						if channel.total_size > 0 {
							writeln!(
								f,
								"\t\t📩 To parachain: {}, {} bytes / {} max",
								peer_parachain, channel.total_size, channel.max_message_size
							)?;
						}
					}
				}
				Ok(())
			},
		}
	}
}

impl Display for DisputesTracker {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		let resolved_time = self
			.resolve_time
			.map_or_else(|| "after unknown number of blocks".to_owned(), |diff| format!("after {diff} blocks"));
		match self.outcome {
			SubxtDisputeResult::Invalid => {
				writeln!(
					f,
					"\t\t👎 Candidate: {}, resolved invalid ({}); voted for: {}; voted against: {}",
					format!("{:?}", self.candidate).dark_red(),
					resolved_time,
					self.voted_for,
					self.voted_against
				)?;
			},
			SubxtDisputeResult::Valid => {
				writeln!(
					f,
					"\t\t👍 Candidate: {}, resolved valid ({}); voted for: {}; voted against: {}",
					format!("{:?}", self.candidate).bright_green(),
					resolved_time,
					self.voted_for,
					self.voted_against
				)?;
			},
			SubxtDisputeResult::TimedOut => {
				writeln!(
					f,
					"\t\t⁉️ Candidate: {}, dispute resolution has been timed out {}; voted for: {}; voted against: {}",
					format!("{:?}", self.candidate).bright_green(),
					resolved_time,
					self.voted_for,
					self.voted_against
				)?;
			},
		}

		if !self.initiators.is_empty() {
			for (validator_idx, validator_address) in &self.initiators {
				writeln!(
					f,
					"\t\t\t😠 Validator initiated dispute: {}",
					format!("idx: {}, address: {}", validator_idx, validator_address).magenta(),
				)?;
			}
		}

		if !self.misbehaving_validators.is_empty() {
			for (validator_idx, validator_address) in &self.misbehaving_validators {
				writeln!(
					f,
					"\t\t\t👹 Validator voted against supermajority: {}",
					format!("idx: {}, address: {}", validator_idx, validator_address).bright_red(),
				)?;
			}
		}

		Ok(())
	}
}
