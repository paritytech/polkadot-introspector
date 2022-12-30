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
use super::{
	progress::{ParachainConsensusEvent, ParachainProgressUpdate},
	prometheus::Metrics,
	stats::ParachainStats,
};
use crate::core::{
	api::{
		AvailabilityBitfield, BackedCandidate, BlockNumber, CoreAssignment, CoreOccupied, InherentData,
		RequestExecutor, ValidatorIndex,
	},
	polkadot::runtime_types::polkadot_primitives::v2::{DisputeStatement, DisputeStatementSet},
	SubxtDisputeResult, SubxtHrmpChannel,
};
use codec::{Decode, Encode};
use log::{debug, error, info};
use std::{
	collections::{BTreeMap, HashMap},
	fmt::Debug,
};
use subxt::ext::{
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
	/// A structure to describe the parachain progress made after processing last relay chain block.
	type ParachainProgressUpdate;
	/// A structure to describe dispute outcome
	type DisputeOutcome;

	/// Injects a new relay chain block into the tracker.
	/// Blocks must be injected in order.
	async fn inject_block(&mut self, block_hash: Self::RelayChainBlockHash) -> &Self::ParachainBlockInfo;

	/// Update current parachain progress.
	fn progress(&mut self, metrics: &Metrics) -> Option<ParachainProgressUpdate>;

	/// Inject disputes resolution results from the tracked events
	fn inject_disputes_events(
		&mut self,
		disputes_tracked: &HashMap<Self::RelayChainBlockHash, (Self::DisputeOutcome, Option<u32>)>,
	);
}

/// Used to track session data, where we store two subsequent sessions: the current and the previous one
struct SubxtSessionTracker {
	/// The current session index
	session_index: u32,
	/// The current session info
	current_keys: Vec<AccountId32>,
	/// The previous session (if available)
	prev_keys: Option<Vec<AccountId32>>,
	/// A flag that indicates a fresh session
	fresh_session: bool,
}

/// An outcome for a dispute
#[derive(Encode, Decode, Debug, Clone)]
pub struct DisputesOutcome {
	/// Disputed candidate
	pub candidate: H256,
	/// The real outcome
	pub outcome: SubxtDisputeResult,
	/// Number of validators voted that a candidate is valid
	pub voted_for: u32,
	/// Number of validators voted that a candidate is invalid
	pub voted_against: u32,
	/// A vector of validators voted against supermajority (index + identify)
	pub misbehaving_validators: Vec<(u32, String)>,
	/// How many blocks have passed since DisputeInitiated event
	pub resolve_time: Option<u32>,
}

#[derive(Default)]
/// A structure that tracks messages (UMP, HRMP, DMP etc)
pub struct SubxtMessageQueuesTracker {
	/// Known inbound HRMP channels, indexed by source parachain id
	pub inbound_hrmp_channels: BTreeMap<u32, SubxtHrmpChannel>,
	/// Known outbound HRMP channels, indexed by source parachain id
	pub outbound_hrmp_channels: BTreeMap<u32, SubxtHrmpChannel>,
}

impl SubxtMessageQueuesTracker {
	/// Update the content of HRMP channels
	pub fn update_hrmp_channels(
		&mut self,
		inbound_channels: BTreeMap<u32, SubxtHrmpChannel>,
		outbound_channels: BTreeMap<u32, SubxtHrmpChannel>,
	) {
		debug!("hrmp channels configured: {:?} in, {:?} out", &inbound_channels, &outbound_channels);
		self.inbound_hrmp_channels = inbound_channels;
		self.outbound_hrmp_channels = outbound_channels;
	}

	/// Returns if there are HRMP messages in any direction
	pub fn has_hrmp_messages(&self) -> bool {
		self.inbound_hrmp_channels.values().any(|channel| channel.total_size > 0) ||
			self.outbound_hrmp_channels.values().any(|channel| channel.total_size > 0)
	}
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
	last_assignment: Option<u32>,
	/// The relay chain block number at which the last candidate was backed at.
	last_backed_at: Option<BlockNumber>,
	/// Information about current block we track.
	current_candidate: ParachainBlockInfo,
	/// Current relay chain block.
	current_relay_block: Option<(BlockNumber, H256)>,
	/// Previous relay chain block
	previous_relay_block: Option<(BlockNumber, H256)>,
	/// Disputes information if any disputes are there.
	disputes: Vec<DisputesOutcome>,
	/// Current relay chain block timestamp.
	current_relay_block_ts: Option<u64>,
	/// Last relay chain block timestamp.
	last_relay_block_ts: Option<u64>,
	/// Last included candidate in relay parent number
	last_included_block: Option<BlockNumber>,
	/// Session tracker
	session_data: Option<SubxtSessionTracker>,
	/// Messages queues status
	message_queues: SubxtMessageQueuesTracker,

	/// Parachain statistics. Used to print summary at the end of a run.
	stats: ParachainStats,
	/// Parachain progress update.
	update: Option<ParachainProgressUpdate>,
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
	/// Parachain availability core assignment information.
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
	type ParachainProgressUpdate = ParachainProgressUpdate;
	type DisputeOutcome = SubxtDisputeResult;

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
				self.message_queues
					.update_hrmp_channels(inbound_hrmp_channels, outbound_hrmp_channels);

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

	fn progress(&mut self, metrics: &Metrics) -> Option<ParachainProgressUpdate> {
		if self.current_relay_block.is_none() {
			// return writeln!(f, "{}", "No relay block processed".to_string().bold().red(),)
			self.update = None;
			return None
		}

		let (relay_block_number, relay_block_hash) = self.current_relay_block.expect("Just checked above; qed");

		self.update = Some(ParachainProgressUpdate {
			para_id: self.para_id,
			timestamp: self.current_relay_block_ts.unwrap_or_default(),
			prev_timestamp: self
				.last_relay_block_ts
				.unwrap_or_else(|| self.current_relay_block_ts.unwrap_or_default()),
			block_number: relay_block_number,
			block_hash: relay_block_hash,
			..Default::default()
		});

		self.progress_core_assignment();
		if let Some(update) = self.update.as_mut() {
			update.core_occupied = self.current_candidate.core_occupied;
		}

		self.update_bitfield_propagation(metrics);

		match self.current_candidate.state {
			ParachainBlockState::Idle => {
				if let Some(update) = self.update.as_mut() {
					update.events.push(ParachainConsensusEvent::SkippedSlot);
				}
				self.stats.on_skipped_slot();
				metrics.on_skipped_slot(self.para_id);
			},
			ParachainBlockState::Backed =>
				if let Some(backed_candidate) = self.current_candidate.candidate.as_ref() {
					let commitments_hash = BlakeTwo256::hash_of(&backed_candidate.candidate.commitments);
					let candidate_hash =
						BlakeTwo256::hash_of(&(&backed_candidate.candidate.descriptor, commitments_hash));
					if let Some(update) = self.update.as_mut() {
						update.events.push(ParachainConsensusEvent::Backed(candidate_hash));
					}
					self.stats.on_backed();
					metrics.on_backed(self.para_id);
				},
			ParachainBlockState::PendingAvailability | ParachainBlockState::Included => {
				self.progress_availability(metrics);
			},
		}

		if let Some(session_data) = self.session_data.as_mut() {
			if session_data.fresh_session {
				session_data.fresh_session = false;
				if let Some(update) = self.update.as_mut() {
					update
						.events
						.push(ParachainConsensusEvent::NewSession(session_data.session_index));
				}
			}
		};

		self.disputes.iter().for_each(|outcome| {
			self.stats.on_disputed(outcome);
			metrics.on_disputed(outcome, self.para_id);
			if let Some(update) = self.update.as_mut() {
				update.events.push(ParachainConsensusEvent::Disputed(outcome.clone()));
			}
		});

		if self.message_queues.has_hrmp_messages() {
			let inbound_channels_active = self
				.message_queues
				.inbound_hrmp_channels
				.iter()
				.filter(|(_, queue)| queue.total_size > 0)
				.map(|(source_id, queue)| (*source_id, queue.clone()))
				.collect::<Vec<_>>();
			let outbound_channels_active = self
				.message_queues
				.outbound_hrmp_channels
				.iter()
				.filter(|(_, queue)| queue.total_size > 0)
				.map(|(dest_id, queue)| (*dest_id, queue.clone()))
				.collect::<Vec<_>>();
			if let Some(update) = self.update.as_mut() {
				update
					.events
					.push(ParachainConsensusEvent::MessageQueues(inbound_channels_active, outbound_channels_active))
			}
		}

		let last_block_number = self.previous_relay_block.unwrap_or((u32::MAX, H256::zero())).0;
		if relay_block_number > last_block_number {
			let tm = self.get_ts();
			self.stats.on_block(tm);
			metrics.on_block(tm.as_secs_f64(), self.para_id);
		}
		self.update.clone()
	}

	fn inject_disputes_events(
		&mut self,
		disputes_tracked: &HashMap<Self::RelayChainBlockHash, (Self::DisputeOutcome, Option<u32>)>,
	) {
		self.disputes.retain_mut(|dispute| -> bool {
			if let Some(observed_dispute) = disputes_tracked.get(&dispute.candidate) {
				if dispute.outcome != observed_dispute.0 {
					info!(
						"Dispute for candidate {:?}: calculated outcome: {:?} is not equal to observed outcome: {:?}",
						&dispute.candidate, dispute.outcome, observed_dispute.0
					);
					dispute.outcome = observed_dispute.0;
				}
				dispute.resolve_time = observed_dispute.1;

				return true
			}

			info!("Dispute for candidate {:?} is not seen via events, remove it", &dispute.candidate);

			false
		});
	}
}

impl SubxtTracker {
	/// Constructor.
	pub fn new(para_id: u32, node_rpc_url: &str, executor: RequestExecutor) -> Self {
		Self {
			para_id,
			node_rpc_url: node_rpc_url.to_owned(),
			executor,
			stats: ParachainStats::new(para_id),
			current_candidate: Default::default(),
			current_relay_block: None,
			previous_relay_block: None,
			current_relay_block_ts: None,
			disputes: Vec::new(),
			last_assignment: None,
			last_backed_at: None,
			last_relay_block_ts: None,
			last_included_block: None,
			message_queues: Default::default(),
			session_data: None,
			update: None,
		}
	}

	fn set_relay_block(&mut self, block_number: BlockNumber, block_hash: H256) {
		self.previous_relay_block = self.current_relay_block;
		self.current_relay_block = Some((block_number, block_hash));
	}

	/// Returns the current relay chain block number.
	pub fn get_current_relay_block_number(&self) -> Option<BlockNumber> {
		self.current_relay_block.map(|block| block.0)
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

				let outcome =
					if voted_for > voted_against { SubxtDisputeResult::Valid } else { SubxtDisputeResult::Invalid };

				let misbehaving_validators = if outcome == SubxtDisputeResult::Valid {
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
					outcome,
					misbehaving_validators,
					resolve_time: None,
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
		self.update = None;
	}

	/// Updates cashed session with a new one, storing the previous session if needed
	fn new_session(&mut self, session_index: u32, account_keys: Vec<AccountId32>) {
		debug!("new session: {}", session_index);
		if let Some(session_data) = &mut self.session_data {
			let old_current = std::mem::replace(&mut session_data.current_keys, account_keys);
			session_data.prev_keys.replace(old_current);
			session_data.session_index = session_index;
			session_data.fresh_session = true;
		} else {
			self.session_data = Some(SubxtSessionTracker {
				session_index,
				current_keys: account_keys,
				prev_keys: None,
				fresh_session: true,
			})
		}
	}

	// TODO: fix this, it is broken, nobody sets this.
	fn progress_core_assignment(&mut self) {
		if let Some(assigned_core) = self.last_assignment {
			if let Some(update) = self.update.as_mut() {
				update.events.push(ParachainConsensusEvent::CoreAssigned(assigned_core));
			}
		}
	}

	fn update_bitfield_propagation(&mut self, metrics: &Metrics) {
		// This makes sense to show if we have a relay chain block and pipeline not idle.
		if let Some((_, _)) = self.current_relay_block {
			// If `max_av_bits` is not set do not check for bitfield propagation.
			// Usually this happens at startup, when we miss a core assignment and we do not update
			// availability before calling this `fn`.
			if self.current_candidate.max_av_bits > 0 &&
				self.current_candidate.state != ParachainBlockState::Idle &&
				self.current_candidate.bitfield_count <= (self.current_candidate.max_av_bits / 3) * 2
			{
				if let Some(update) = self.update.as_mut() {
					update.events.push(ParachainConsensusEvent::SlowBitfieldPropagation(
						self.current_candidate.bitfield_count,
						self.current_candidate.max_av_bits,
					))
				}
				self.stats.on_bitfields(self.current_candidate.bitfield_count, true);
				metrics.on_bitfields(self.current_candidate.bitfield_count, true, self.para_id);
			} else {
				self.stats.on_bitfields(self.current_candidate.bitfield_count, false);
				metrics.on_bitfields(self.current_candidate.bitfield_count, true, self.para_id);
			}
		}
	}

	fn progress_availability(&mut self, metrics: &Metrics) {
		let (relay_block_number, _) = self.current_relay_block.expect("Checked by caller; qed");

		// Update bitfields health.
		if let Some(update) = self.update.as_mut() {
			update.bitfield_health.max_bitfield_count = self.current_candidate.max_av_bits;
			update.bitfield_health.available_count = self.current_candidate.current_av_bits;
			update.bitfield_health.bitfield_count = self.current_candidate.bitfield_count;
		}

		// TODO: Availability timeout.
		if self.current_candidate.current_av_bits > (self.current_candidate.max_av_bits / 3) * 2 {
			if let Some(backed_candidate) = self.current_candidate.candidate.as_ref() {
				let commitments_hash = BlakeTwo256::hash_of(&backed_candidate.candidate.commitments);
				let candidate_hash = BlakeTwo256::hash_of(&(&backed_candidate.candidate.descriptor, commitments_hash));
				if let Some(update) = self.update.as_mut() {
					update.events.push(ParachainConsensusEvent::Included(
						candidate_hash,
						self.current_candidate.current_av_bits,
						self.current_candidate.max_av_bits,
					));
				}
				self.stats.on_included(relay_block_number, self.last_included_block);
				metrics.on_included(relay_block_number, self.last_included_block, self.para_id);
				self.last_included_block = Some(relay_block_number);
			}
		} else if self.current_candidate.core_occupied && self.last_backed_at != Some(relay_block_number) {
			if let Some(update) = self.update.as_mut() {
				update.events.push(ParachainConsensusEvent::SlowAvailability(
					self.current_candidate.current_av_bits,
					self.current_candidate.max_av_bits,
				));
			}
			self.stats.on_slow_availability();
			metrics.on_slow_availability(self.para_id);
		}
	}

	/// Returns the time for the current block
	pub fn get_ts(&self) -> std::time::Duration {
		let cur_ts = self.current_relay_block_ts.unwrap_or_default();
		let base_ts = self.last_relay_block_ts.unwrap_or(cur_ts);
		std::time::Duration::from_millis(cur_ts).saturating_sub(std::time::Duration::from_millis(base_ts))
	}

	/// Returns the current session index if present
	pub fn get_current_session_index(&self) -> Option<u32> {
		self.session_data.as_ref().map(|session| session.session_index)
	}

	/// Returns the stats
	pub fn summary(&self) -> &ParachainStats {
		&self.stats
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
