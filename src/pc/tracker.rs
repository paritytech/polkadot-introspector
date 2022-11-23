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
use crate::core::{
	api::{
		AvailabilityBitfield, BackedCandidate, BlockNumber, CoreAssignment, CoreOccupied, InherentData,
		RequestExecutor, ValidatorIndex,
	},
	polkadot::runtime_types::polkadot_primitives::v2::{DisputeStatement, DisputeStatementSet},
};
use codec::{Decode, Encode};
use color_eyre::owo_colors::OwoColorize;
use crossterm::style::Stylize;
use log::{debug, error};
use std::{
	collections::BTreeMap,
	fmt,
	fmt::{Debug, Display},
};
use subxt::{
	sp_core::{crypto::AccountId32, H256},
	sp_runtime::traits::{BlakeTwo256, Hash},
};

/// An abstract definition of a parachain block tracker.
#[async_trait::async_trait]
pub trait ParachainBlockTracker {
	/// The relay chain block hash
	type RelayChainBlockHash;
	/// The relay chain block type.
	type RelayChainBlockNumber;
	/// The parachain inherent data.
	type ParaInherentData;
	/// The state obtained from processing a block.
	type ParachainBlockInfo;

	/// Injects a new relay chain block into the tracker.
	/// Blocks must be injected in order.
	async fn inject_block(&mut self, block_hash: Self::RelayChainBlockHash) -> &Self::ParachainBlockInfo;
}

/// Used to track session data, where we store two subsequent sessions: the current and the previous one
struct SubxtSessionTracker {
	/// The current session index
	session_index: u32,
	/// The current session info
	current_keys: Vec<AccountId32>,
	/// The previous session (if available)
	prev_keys: Option<Vec<AccountId32>>,
}

/// A subxt based parachain candidate tracker.
pub struct SubxtTracker {
	/// Parachain ID to track.
	para_id: u32,
	/// RPC node endpoint.
	node_rpc_url: String,
	/// A subxt API wrapper.
	executor: RequestExecutor,
	/// The last availability core index the parachain has been assigned to.
	last_assignment: Option<usize>,
	/// The relay chain block number at which the last candidate was backed at.
	last_backed_at: Option<BlockNumber>,
	/// Information about current block we track.
	current_candidate: ParachainBlockInfo,
	/// Current relay chain block.
	current_relay_block: Option<(BlockNumber, H256)>,
	/// Disputes information if any disputes are there.
	disputes: Vec<DisputesOutcome>,
	/// Current relay chain block timestamp.
	current_relay_block_ts: Option<u64>,
	/// Last relay chain block timestamp.
	last_relay_block_ts: Option<u64>,
	/// Session tracker
	session_data: Option<SubxtSessionTracker>,
	/// Known inbound HRMP channels, indexed by source parachain id
	inbound_hrmp_channels: Vec<u32>,
	/// Inbound hrmp content for the parachain, indexed by sender parachain id
	inbound_hrmp_messages: BTreeMap<u32, Vec<u8>>,
	/// Known outbound HRMP channels, indexed by source parachain id
	outbound_hrmp_channels: Vec<u32>,
	/// Inbound hrmp content for the parachain, indexed by sender parachain id
	outbound_hrmp_messages: BTreeMap<u32, Vec<u8>>,
}

impl Display for SubxtTracker {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		if self.current_relay_block.is_none() {
			return writeln!(f, "{}", "No relay block processed".to_string().bold().red(),)
		}
		self.display_bitfield_propagation(f)?;

		let (relay_block_number, _) = self.current_relay_block.unwrap();
		match self.current_candidate.state {
			ParachainBlockState::Idle => {
				writeln!(
					f,
					"{} for parachain {}, no candidate backed",
					format!("{} [#{}] SLOW BACKING", self.format_ts(), relay_block_number)
						.bold()
						.red(),
					self.para_id,
				)?;
			},
			ParachainBlockState::Backed => {
				writeln!(
					f,
					"{}",
					format!("{} [#{}] CANDIDATE BACKED", self.format_ts(), relay_block_number)
						.bold()
						.green(),
				)?;
			},
			ParachainBlockState::PendingAvailability | ParachainBlockState::Included => {
				self.display_availability(f)?;
			},
		}

		self.display_block_info(f)?;
		self.display_core_assignment(f)?;
		self.display_core_status(f)
	}
}

#[derive(Encode, Decode, Debug, Default)]
struct DisputesOutcome {
	candidate: H256,
	voted_for: u32,
	voted_against: u32,
	misbehaving_validators: Vec<(u32, String)>,
}

/// The parachain block tracking information.
/// This is used for displaying CLI updates and also goes to Storage.
#[derive(Encode, Decode, Debug, Default)]
pub struct ParachainBlockInfo {
	/// The candidate information as observed during backing
	candidate: Option<BackedCandidate<H256>>,
	/// The current state.
	state: ParachainBlockState,
	/// The number of signed bitfields.
	bitfield_count: u32,
	/// The maximum expected number of availability bits that can be set. Corresponds to `max_validators`.
	max_av_bits: u32,
	/// The current number of observed availability bits set to 1.
	current_av_bits: u32,
	/// Parachain availability core asignment information.
	assigned_core: Option<u32>,
	/// Core occupation status.
	core_occupied: bool,
}

/// The state of parachain block.
#[derive(Encode, Decode, Debug, Default, Clone, PartialEq, Eq)]
enum ParachainBlockState {
	// Parachain block pipeline is idle.
	#[default]
	Idle,
	// A candidate is currently backed.
	Backed,
	// A candidate is pending inclusion.
	PendingAvailability,
	// A candidate has been included.
	Included,
}

#[async_trait::async_trait]
impl ParachainBlockTracker for SubxtTracker {
	type RelayChainBlockHash = H256;
	type RelayChainBlockNumber = BlockNumber;
	type ParaInherentData = InherentData;
	type ParachainBlockInfo = ParachainBlockInfo;

	async fn inject_block(&mut self, block_hash: Self::RelayChainBlockHash) -> &Self::ParachainBlockInfo {
		if let Some(header) = self.executor.get_block_head(self.node_rpc_url.clone(), Some(block_hash)).await {
			if let Some(inherent) = self
				.executor
				.extract_parainherent_data(self.node_rpc_url.clone(), Some(block_hash))
				.await
			{
				self.set_relay_block(header.number, block_hash);
				let inbound_hrmp_channels = self
					.executor
					.get_inbound_hrmp_channels(self.node_rpc_url.clone(), block_hash, self.para_id)
					.await;
				let outbound_hrmp_channels = self
					.executor
					.get_outbound_hrmp_channels(self.node_rpc_url.clone(), block_hash, self.para_id)
					.await;
				self.update_hrmp_channels(inbound_hrmp_channels, outbound_hrmp_channels);

				for channel in &self.inbound_hrmp_channels {
					let content = self
						.executor
						.get_hrmp_content(self.node_rpc_url.clone(), block_hash, self.para_id, *channel)
						.await;
					self.inbound_hrmp_messages
						.insert(*channel, content.into_iter().flatten().collect());
				}
				for channel in &self.outbound_hrmp_channels {
					let content = self
						.executor
						.get_hrmp_content(self.node_rpc_url.clone(), block_hash, *channel, self.para_id)
						.await;
					self.outbound_hrmp_messages
						.insert(*channel, content.into_iter().flatten().collect());
				}

				let cur_session = self.executor.get_session_index(self.node_rpc_url.clone(), block_hash).await;
				if let Some(stored_session) = self.get_current_session_index() {
					if cur_session != stored_session {
						self.new_session(
							cur_session,
							self.executor
								.get_session_account_keys(self.node_rpc_url.clone(), cur_session)
								.await
								.unwrap(),
						)
					}
				} else {
					self.new_session(
						cur_session,
						self.executor
							.get_session_account_keys(self.node_rpc_url.clone(), cur_session)
							.await
							.unwrap(),
					)
				}
				self.on_inherent_data(block_hash, header.number, inherent).await;
			} else {
				error!("Failed to get inherent data for {:?}", block_hash);
			}
		} else {
			error!("Failed to get block header for {:?}", block_hash);
		}

		&self.current_candidate
	}
}

impl SubxtTracker {
	/// Constructor.
	pub fn new(para_id: u32, node_rpc_url: String, executor: RequestExecutor) -> Self {
		Self {
			para_id,
			node_rpc_url,
			executor,
			current_candidate: Default::default(),
			last_assignment: None,
			last_backed_at: None,
			current_relay_block: None,
			disputes: vec![],
			current_relay_block_ts: None,
			last_relay_block_ts: None,
			session_data: None,
			inbound_hrmp_channels: vec![],
			inbound_hrmp_messages: BTreeMap::new(),
			outbound_hrmp_channels: vec![],
			outbound_hrmp_messages: BTreeMap::new(),
		}
	}

	fn set_relay_block(&mut self, block_number: BlockNumber, block_hash: H256) {
		self.current_relay_block = Some((block_number, block_hash));
	}

	fn get_session_keys(&self, session_index: u32) -> Option<&Vec<AccountId32>> {
		self.session_data.as_ref().and_then(|session_data| {
			if session_data.session_index == session_index {
				Some(&session_data.current_keys)
			} else if session_data.session_index - 1 == session_index {
				session_data.prev_keys.as_ref()
			} else {
				None
			}
		})
	}

	// Parse inherent data and update state.
	async fn on_inherent_data(&mut self, block_hash: H256, block_number: BlockNumber, data: InherentData) {
		let core_assignments = self.executor.get_scheduled_paras(self.node_rpc_url.clone(), block_hash).await;
		let backed_candidates = data.backed_candidates;
		let occupied_cores = self.executor.get_occupied_cores(self.node_rpc_url.clone(), block_hash).await;
		let validator_groups = self.executor.get_backing_groups(self.node_rpc_url.clone(), block_hash).await;
		let bitfields = data
			.bitfields
			.into_iter()
			.map(|b| b.payload)
			.collect::<Vec<AvailabilityBitfield>>();

		self.current_candidate.bitfield_count = bitfields.len() as u32;
		self.last_relay_block_ts = self.current_relay_block_ts;
		self.current_relay_block_ts = Some(
			self.executor
				.get_block_timestamp(self.node_rpc_url.clone(), Some(block_hash))
				.await,
		);

		// Update backing information if any.
		let candidate_backed = self.update_backing(backed_candidates, block_number);
		self.update_core_assignment(core_assignments);

		if let Some(assigned_core) = self.current_candidate.assigned_core {
			self.update_core_occupation(assigned_core, occupied_cores);
		}

		if !data.disputes.is_empty() {
			self.update_disputes(&data.disputes[..]);
		}

		// If a candidate was backed in this relay block, we don't need to process availability now.
		if candidate_backed {
			return
		}

		if self.current_candidate.candidate.is_none() {
			// If no candidate is being backed reset the state to `Idle`.
			self.current_candidate.state = ParachainBlockState::Idle;
			return
		}

		// We only process availability if our parachain is assigned to an availability core.
		if let Some(assigned_core) = self.current_candidate.assigned_core {
			self.update_availability(assigned_core, bitfields, validator_groups);
		}
	}

	fn update_backing(&mut self, mut backed_candidates: Vec<BackedCandidate<H256>>, block_number: BlockNumber) -> bool {
		let candidate_index = backed_candidates
			.iter()
			.position(|candidate| candidate.candidate.descriptor.para_id.0 == self.para_id);

		// Update the curent state if a candiate was backed for this para.
		if let Some(index) = candidate_index {
			self.current_candidate.state = ParachainBlockState::Backed;
			self.current_candidate.candidate = Some(backed_candidates.remove(index));
			self.last_backed_at = Some(block_number);

			true
		} else {
			false
		}
	}

	fn update_core_assignment(&mut self, core_assignments: Vec<CoreAssignment>) {
		if let Some(index) = core_assignments
			.iter()
			.position(|assignment| assignment.para_id.0 == self.para_id)
		{
			self.current_candidate.assigned_core = Some(core_assignments[index].core.0);
		}
	}
	fn update_core_occupation(&mut self, core: u32, occupied_cores: Vec<Option<CoreOccupied>>) {
		self.current_candidate.core_occupied = occupied_cores[core as usize].is_some();
	}

	fn update_disputes(&mut self, disputes: &[DisputeStatementSet]) {
		self.disputes = disputes
			.iter()
			.map(|dispute_info| {
				let session_index = dispute_info.session;
				let session_info = self.get_session_keys(session_index);
				// TODO: we would like to distinguish different dispute phases at some point
				let voted_for = dispute_info
					.statements
					.iter()
					.filter(|(statement, _, _)| matches!(statement, DisputeStatement::Valid(_)))
					.count() as u32;
				let voted_against = dispute_info.statements.len() as u32 - voted_for;

				let misbehaving_validators = if voted_for > voted_against {
					dispute_info
						.statements
						.iter()
						.filter(|(statement, _, _)| !matches!(statement, DisputeStatement::Valid(_)))
						.map(|(_, idx, _)| extract_validator_address(session_info, idx.0))
						.collect()
				} else {
					dispute_info
						.statements
						.iter()
						.filter(|(statement, _, _)| matches!(statement, DisputeStatement::Valid(_)))
						.map(|(_, idx, _)| extract_validator_address(session_info, idx.0))
						.collect()
				};
				DisputesOutcome {
					candidate: dispute_info.candidate_hash.0,
					voted_for,
					voted_against,
					misbehaving_validators,
				}
			})
			.collect();
	}

	fn update_availability(
		&mut self,
		core: u32,
		bitfields: Vec<AvailabilityBitfield>,
		validator_groups: Vec<Vec<ValidatorIndex>>,
	) {
		let avail_bits: u32 = bitfields
			.iter()
			.map(|bitfield| {
				let bit = bitfield.0[core as usize];
				bit as u32
			})
			.sum();

		let all_bits = validator_groups.into_iter().flatten().collect::<Vec<ValidatorIndex>>();

		self.current_candidate.max_av_bits = all_bits.len() as u32;
		self.current_candidate.current_av_bits = avail_bits;
		self.current_candidate.state = ParachainBlockState::PendingAvailability;

		// Check availability and update state accordingly.
		if avail_bits > (all_bits.len() as u32 / 3) * 2 {
			self.current_candidate.state = ParachainBlockState::Included;
		}
	}

	/// Called to move to idle state after inclusion/timeout.
	pub fn maybe_reset_state(&mut self) {
		if self.current_candidate.state == ParachainBlockState::Included {
			self.current_candidate.state = ParachainBlockState::Idle;
			self.current_candidate.candidate = None;
		}
		self.disputes.clear();
	}

	/// Updates cashed session with a new one, storing the previous session if needed
	fn new_session(&mut self, session_index: u32, account_keys: Vec<AccountId32>) {
		debug!("new session: {}", session_index);
		if let Some(session_data) = &mut self.session_data {
			let old_current = std::mem::replace(&mut session_data.current_keys, account_keys);
			session_data.prev_keys.replace(old_current);
			session_data.session_index = session_index;
		} else {
			self.session_data = Some(SubxtSessionTracker { session_index, current_keys: account_keys, prev_keys: None })
		}
	}

	fn update_hrmp_channels(&mut self, inbound_channels: Vec<u32>, outbound_channels: Vec<u32>) {
		debug!("hrmp channels configured: {:?} in, {:?} out", &inbound_channels, &outbound_channels);
		self.inbound_hrmp_channels = inbound_channels;
		self.outbound_hrmp_channels = outbound_channels;
		self.inbound_hrmp_messages.clear();
		self.outbound_hrmp_messages.clear();
	}

	fn display_core_assignment(&self, f: &mut fmt::Formatter) -> fmt::Result {
		if let Some(assigned_core) = self.last_assignment {
			writeln!(f, "\t- Parachain {} assigned to core index {}", self.para_id, assigned_core)
		} else {
			Ok(())
		}
	}

	fn display_bitfield_propagation(&self, f: &mut fmt::Formatter) -> fmt::Result {
		// This makes sense to show if we have a relay chain block and pipeline not idle.
		if let Some((relay_block_number, _)) = self.current_relay_block {
			// If `max_av_bits` is not set do not check for bitfield propagation.
			// Usually this happens at startup, when we miss a core assignment and we do not update
			// availability before calling this `fn`.
			if self.current_candidate.max_av_bits > 0 &&
				self.current_candidate.state != ParachainBlockState::Idle &&
				self.current_candidate.bitfield_count <= (self.current_candidate.max_av_bits / 3) * 2
			{
				writeln!(
					f,
					"{} bitfield count {}/{}",
					format!("{} [#{}] SLOW BITFIELD PROPAGATION", self.format_ts(), relay_block_number).dark_red(),
					self.current_candidate.bitfield_count,
					self.current_candidate.max_av_bits
				)?;
			}
		}

		Ok(())
	}

	fn display_availability(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let (relay_block_number, _) = self.current_relay_block.unwrap();

		// TODO: Availability timeout.
		if self.current_candidate.current_av_bits > (self.current_candidate.max_av_bits / 3) * 2 {
			writeln!(
				f,
				"{}",
				format!("{} [#{}] CANDIDATE INCLUDED", self.format_ts(), relay_block_number)
					.bold()
					.green(),
			)?;
		} else if self.current_candidate.core_occupied && self.last_backed_at != Some(relay_block_number) {
			writeln!(
				f,
				"{}",
				format!("{} [#{}] SLOW AVAILABILITY", self.format_ts(), relay_block_number)
					.bold()
					.yellow(),
			)?;
		}

		writeln!(
			f,
			"\t🟢 Availability bits: {}/{}",
			self.current_candidate.current_av_bits, self.current_candidate.max_av_bits
		)
	}

	fn display_core_status(&self, f: &mut fmt::Formatter) -> fmt::Result {
		writeln!(
			f,
			"\t🥝 Availability core {}",
			if !self.current_candidate.core_occupied { "FREE" } else { "OCCUPIED" }
		)
	}

	fn display_disputes(&self, f: &mut fmt::Formatter) -> fmt::Result {
		writeln!(f, "\t👊 Disputes tracked")?;
		for dispute in &self.disputes {
			if dispute.voted_for < dispute.voted_against {
				writeln!(
					f,
					"\t\t👎 Candidate: {}, resolved invalid; voted for: {}; voted against: {}",
					format!("{:?}", dispute.candidate).dark_red(),
					dispute.voted_for,
					dispute.voted_against
				)?;
			} else {
				writeln!(
					f,
					"\t\t👍 Candidate: {}, resolved valid; voted for: {}; voted against: {}",
					format!("{:?}", dispute.candidate).bright_green(),
					dispute.voted_for,
					dispute.voted_against
				)?;
			}

			if !dispute.misbehaving_validators.is_empty() {
				for validator in &dispute.misbehaving_validators {
					writeln!(
						f,
						"\t\t\t👹 Validator voted against supermajority: {}",
						format!("idx: {}, address: {}", validator.0, validator.1).bright_red(),
					)?;
				}
			}
		}
		Ok(())
	}

	fn display_hrmp(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let total: usize = self.inbound_hrmp_messages.values().map(|data| data.len()).sum();

		if total > 0 {
			writeln!(f, "\t👉 Inbound HRMP messages, received {} bytes in total", total)?;

			for (input_parachain, data) in self.inbound_hrmp_messages.iter() {
				writeln!(f, "\t\t📩 From parachain: {}, {} bytes", input_parachain, data.len())?;
			}
		}

		let total: usize = self.outbound_hrmp_messages.values().map(|data| data.len()).sum();

		if total > 0 {
			writeln!(f, "\t👈 Outbound HRMP messages, sent {} bytes in total", total)?;

			for (output, data) in self.outbound_hrmp_messages.iter() {
				writeln!(f, "\t\t📩 To parachain: {}, {} bytes", output, data.len())?;
			}
		}

		Ok(())
	}

	fn display_block_info(&self, f: &mut fmt::Formatter) -> fmt::Result {
		if let Some(backed_candidate) = self.current_candidate.candidate.as_ref() {
			let commitments_hash = BlakeTwo256::hash_of(&backed_candidate.candidate.commitments);
			let candidate_hash = BlakeTwo256::hash_of(&(&backed_candidate.candidate.descriptor, commitments_hash));

			writeln!(f, "\t💜 Candidate hash: {} ", format!("{:?}", candidate_hash).magenta())?;
		}

		if let Some((_, relay_block_hash)) = self.current_relay_block {
			writeln!(f, "\t🔗 Relay block hash: {} ", format!("{:?}", relay_block_hash).bold())?;
		}

		if !self.disputes.is_empty() {
			self.display_disputes(f)?;
		}

		if self.inbound_hrmp_messages.values().any(|data| !data.is_empty()) ||
			self.outbound_hrmp_messages.values().any(|data| !data.is_empty())
		{
			self.display_hrmp(f)?;
		}

		Ok(())
	}

	/// Format the current block inherent timestamp.
	pub fn format_ts(&self) -> String {
		let cur_ts = self.current_relay_block_ts.unwrap_or_default();
		let base_ts = self.last_relay_block_ts.unwrap_or(cur_ts);
		let duration =
			std::time::Duration::from_millis(cur_ts).saturating_sub(std::time::Duration::from_millis(base_ts));

		let dt = time::OffsetDateTime::from_unix_timestamp_nanos(
			self.current_relay_block_ts.unwrap_or_default() as i128 * 1_000_000,
		)
		.unwrap();
		format!(
			"{} +{}",
			dt.format(&time::format_description::well_known::Iso8601::DEFAULT)
				.expect("Invalid datetime format"),
			format_args!("{}ms", duration.as_millis())
		)
	}

	/// Returns the current session index if present
	pub fn get_current_session_index(&self) -> Option<u32> {
		self.session_data.as_ref().map(|session| session.session_index)
	}
}

// Examines session info (if any) and find the corresponding validator
fn extract_validator_address(session_keys: Option<&Vec<AccountId32>>, validator_index: u32) -> (u32, String) {
	if let Some(session_keys) = session_keys.as_ref() {
		if validator_index < session_keys.len() as u32 {
			let validator_identity = &session_keys[validator_index as usize];
			(validator_index, validator_identity.to_string())
		} else {
			(
				validator_index,
				format!("??? (no such validator index {}: know {} validators)", validator_index, session_keys.len()),
			)
		}
	} else {
		(validator_index, "??? (no session keys)".to_string())
	}
}
