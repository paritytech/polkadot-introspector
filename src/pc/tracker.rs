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
	collector::{CollectorPrefixType, CollectorStorageApi, DisputeInfo},
	polkadot::runtime_types::polkadot_primitives::v2::{DisputeStatement, DisputeStatementSet},
	SubxtDisputeResult, SubxtHrmpChannel,
};
use codec::{Decode, Encode};
use log::{debug, error, info, warn};
use std::{collections::BTreeMap, default::Default, fmt::Debug};
use subxt::{
	config::{substrate::BlakeTwo256, Hasher},
	utils::{AccountId32, H256},
};

/// An abstract definition of a parachain block tracker.
#[async_trait::async_trait]
pub trait ParachainBlockTracker {
	/// The relay chain block hash
	type RelayChainNewHead;
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
	async fn inject_block(
		&mut self,
		block_hash: Self::RelayChainNewHead,
		block_number: Self::RelayChainBlockNumber,
	) -> color_eyre::Result<&Self::ParachainBlockInfo>;
	/// Called when a new session is observed
	async fn new_session(&mut self, new_session_index: u32);

	/// Update current parachain progress.
	fn progress(&mut self, metrics: &Metrics) -> Option<ParachainProgressUpdate>;
}

/// An outcome for a dispute
#[derive(Encode, Decode, Debug, Clone)]
pub struct DisputesTracker {
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
	/// Dispute conclusion time: how many blocks have passed since DisputeInitiated event
	pub resolve_time: Option<u32>,
}

/// Used to track forks of the relay chain
#[derive(Debug, Clone)]
struct ForkTracker {
	#[allow(dead_code)]
	relay_hash: H256,
	#[allow(dead_code)]
	relay_number: u32,
	backed_candidate: Option<H256>,
	included_candidate: Option<H256>,
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
	/// API to access collector's storage
	api: CollectorStorageApi,
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
	disputes: Vec<DisputesTracker>,
	/// Current relay chain block timestamp.
	current_relay_block_ts: Option<u64>,
	/// Last observed finality lag
	finality_lag: Option<u32>,
	/// Last relay chain block timestamp.
	last_relay_block_ts: Option<u64>,
	/// Last included candidate in relay parent number
	last_included_block: Option<BlockNumber>,
	/// Messages queues status
	message_queues: SubxtMessageQueuesTracker,

	/// Parachain statistics. Used to print summary at the end of a run.
	stats: ParachainStats,
	/// Parachain progress update.
	update: Option<ParachainProgressUpdate>,
	/// Current forks recorded
	relay_forks: Vec<ForkTracker>,
}

/// The parachain block tracking information.
/// This is used for displaying CLI updates and also goes to Storage.
#[derive(Encode, Decode, Debug, Default)]
pub struct ParachainBlockInfo {
	/// The candidate information as observed during backing
	candidate: Option<BackedCandidate<H256>>,
	/// Candidate hash
	candidate_hash: Option<H256>,
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
	type RelayChainNewHead = H256;
	type RelayChainBlockNumber = BlockNumber;
	type ParaInherentData = InherentData;
	type ParachainBlockInfo = ParachainBlockInfo;
	type ParachainProgressUpdate = ParachainProgressUpdate;
	type DisputeOutcome = SubxtDisputeResult;

	async fn inject_block(
		&mut self,
		block_hash: Self::RelayChainNewHead,
		block_number: Self::RelayChainBlockNumber,
	) -> color_eyre::Result<&Self::ParachainBlockInfo> {
		let is_fork = block_number == self.current_relay_block.unwrap_or((0, H256::zero())).0;
		let inherent_data = self
			.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::InherentData, block_hash)
			.await;

		if let Some(inherent_data) = inherent_data {
			let inherent: InherentData = inherent_data.into_inner().unwrap();
			self.set_relay_block(block_number, block_hash);

			let inbound_hrmp_channels = self
				.executor
				.get_inbound_hrmp_channels(self.node_rpc_url.as_str(), block_hash, self.para_id)
				.await?;
			let outbound_hrmp_channels = self
				.executor
				.get_outbound_hrmp_channels(self.node_rpc_url.as_str(), block_hash, self.para_id)
				.await?;
			self.message_queues
				.update_hrmp_channels(inbound_hrmp_channels, outbound_hrmp_channels);
			self.on_inherent_data(block_hash, block_number, inherent, is_fork).await?;
		} else {
			error!("Failed to get inherent data for {:?}", block_hash);
		}

		Ok(&self.current_candidate)
	}

	async fn new_session(&mut self, new_session_index: u32) {
		if let Some(update) = self.update.as_mut() {
			update.events.push(ParachainConsensusEvent::NewSession(new_session_index));
		}
	}

	fn progress(&mut self, metrics: &Metrics) -> Option<ParachainProgressUpdate> {
		if self.current_relay_block.is_none() {
			// return writeln!(f, "{}", "No relay block processed".to_string().bold().red(),)
			self.update = None;
			return None
		}

		let (relay_block_number, relay_block_hash) = self.current_relay_block.expect("Just checked above; qed");
		let is_fork = relay_block_number == self.previous_relay_block.unwrap_or((0, H256::zero())).0;

		self.update = Some(ParachainProgressUpdate {
			para_id: self.para_id,
			timestamp: self.current_relay_block_ts.unwrap_or_default(),
			prev_timestamp: self
				.last_relay_block_ts
				.unwrap_or_else(|| self.current_relay_block_ts.unwrap_or_default()),
			block_number: relay_block_number,
			block_hash: relay_block_hash,
			is_fork,
			finality_lag: self.finality_lag,
			..Default::default()
		});

		self.progress_core_assignment();
		if let Some(update) = self.update.as_mut() {
			update.core_occupied = self.current_candidate.core_occupied;
		}

		self.update_bitfield_propagation(metrics);

		match self.current_candidate.state {
			ParachainBlockState::Idle =>
				if let Some(update) = self.update.as_mut() {
					update.events.push(ParachainConsensusEvent::SkippedSlot);
					self.stats.on_skipped_slot(update);
					metrics.on_skipped_slot(update);
				},
			ParachainBlockState::Backed =>
				if let Some(candidate_hash) = self.current_candidate.candidate_hash {
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

		if let Some(finality_lag) = self.finality_lag {
			metrics.on_finality_lag(finality_lag);
		}

		self.update.clone()
	}
}

impl SubxtTracker {
	/// Constructor.
	///
	/// # Arguments
	///
	/// * `last_skipped_slot_blocks` - The number of last blocks with missing slots to display in cli stats
	pub fn new(
		para_id: u32,
		node_rpc_url: &str,
		executor: RequestExecutor,
		api: CollectorStorageApi,
		last_skipped_slot_blocks: usize,
	) -> Self {
		Self {
			para_id,
			node_rpc_url: node_rpc_url.to_owned(),
			executor,
			api,
			stats: ParachainStats::new(para_id, last_skipped_slot_blocks),
			current_candidate: Default::default(),
			current_relay_block: None,
			previous_relay_block: None,
			current_relay_block_ts: None,
			finality_lag: None,
			disputes: Vec::new(),
			last_assignment: None,
			last_backed_at: None,
			last_relay_block_ts: None,
			last_included_block: None,
			message_queues: Default::default(),
			update: None,
			relay_forks: vec![],
		}
	}

	fn set_relay_block(&mut self, block_number: BlockNumber, block_hash: H256) {
		self.previous_relay_block = self.current_relay_block;
		self.current_relay_block = Some((block_number, block_hash));
	}

	async fn get_session_keys(&self, session_index: u32) -> Option<Vec<AccountId32>> {
		let session_hash = BlakeTwo256::hash(&session_index.to_be_bytes()[..]);
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::AccountKeys, session_hash)
			.await
			.map(|session_entry| session_entry.into_inner().unwrap())
	}

	// Parse inherent data and update state.
	async fn on_inherent_data(
		&mut self,
		block_hash: H256,
		block_number: BlockNumber,
		data: InherentData,
		is_fork: bool,
	) -> color_eyre::Result<()> {
		let core_assignments = self
			.executor
			.get_scheduled_paras(self.node_rpc_url.as_str(), block_hash)
			.await?;
		let backed_candidates = data.backed_candidates;
		let occupied_cores = self.executor.get_occupied_cores(self.node_rpc_url.as_str(), block_hash).await?;
		let validator_groups = self.executor.get_backing_groups(self.node_rpc_url.as_str(), block_hash).await?;
		let bitfields = data
			.bitfields
			.into_iter()
			.map(|b| b.payload)
			.collect::<Vec<AvailabilityBitfield>>();

		self.current_candidate.bitfield_count = bitfields.len() as u32;

		if !is_fork {
			self.last_relay_block_ts = self.current_relay_block_ts;
			self.relay_forks.clear();
		}

		self.relay_forks.push(ForkTracker {
			relay_hash: block_hash,
			relay_number: block_number,
			included_candidate: None,
			backed_candidate: None,
		});

		self.current_relay_block_ts = Some(
			self.executor
				.get_block_timestamp(self.node_rpc_url.as_str(), Some(block_hash))
				.await?,
		);

		if let Some((relay_block_number, relay_block_hash)) = self.current_relay_block {
			let maybe_relevant_finalized_block_number = self
				.api
				.storage()
				.storage_read_prefixed(CollectorPrefixType::RelevantFinalizedBlockNumber, relay_block_hash)
				.await;
			self.finality_lag = match maybe_relevant_finalized_block_number {
				Some(entry) => match entry.into_inner::<u32>() {
					Ok(relevant_finalized_block_number) => Some(relay_block_number - relevant_finalized_block_number),
					Err(e) => {
						warn!("Cannot decode the value of finality_lag: {:?}", e);
						None
					},
				},
				None => None,
			};
		}

		// Update backing information if any.
		let candidate_backed = self.update_backing(backed_candidates, block_number);
		self.update_core_assignment(core_assignments);

		if let Some(assigned_core) = self.current_candidate.assigned_core {
			self.update_core_occupation(assigned_core, occupied_cores);
		}

		if !data.disputes.is_empty() {
			self.update_disputes(&data.disputes[..]).await;
		}

		// If a candidate was backed in this relay block, we don't need to process availability now.
		if candidate_backed {
			if let Some(current_fork) = self.relay_forks.last_mut() {
				current_fork.backed_candidate = Some(
					self.current_candidate
						.candidate_hash
						.expect("checked in candidate_backed, qed."),
				);
			}
			return Ok(())
		}

		if self.current_candidate.candidate.is_none() &&
			self.relay_forks
				.iter()
				.all(|fork| fork.backed_candidate.is_none() && fork.included_candidate.is_none())
		{
			// If no candidate is being backed reset the state to `Idle`.
			self.current_candidate.state = ParachainBlockState::Idle;
			return Ok(())
		}

		if self.current_candidate.state == ParachainBlockState::Backed {
			// We only process availability if our parachain is assigned to an availability core.
			if let Some(assigned_core) = self.current_candidate.assigned_core {
				self.update_availability(assigned_core, bitfields, validator_groups);
			}
		}

		Ok(())
	}

	fn update_backing(&mut self, mut backed_candidates: Vec<BackedCandidate<H256>>, block_number: BlockNumber) -> bool {
		let candidate_index = backed_candidates
			.iter()
			.position(|candidate| candidate.candidate.descriptor.para_id.0 == self.para_id);

		// Update the curent state if a candiate was backed for this para.
		if let Some(index) = candidate_index {
			self.current_candidate.state = ParachainBlockState::Backed;
			let backed_candidate = backed_candidates.remove(index);
			let commitments_hash = BlakeTwo256::hash_of(&backed_candidate.candidate.commitments);
			self.current_candidate.candidate_hash =
				Some(BlakeTwo256::hash_of(&(&backed_candidate.candidate.descriptor, commitments_hash)));
			self.current_candidate.candidate = Some(backed_candidate);
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

	async fn update_disputes(&mut self, disputes: &[DisputeStatementSet]) {
		self.disputes = Vec::with_capacity(disputes.len());
		for dispute_info in disputes {
			let stored_dispute = self
				.api
				.storage()
				.storage_read_prefixed(CollectorPrefixType::Dispute(self.para_id), dispute_info.candidate_hash.0)
				.await;
			if let Some(stored_dispute) = stored_dispute.map(|entry| -> DisputeInfo { entry.into_inner().unwrap() }) {
				if stored_dispute.outcome.is_none() {
					info!("dispute for candidate {} has been seen in the block inherent but is not tracked to be resolved",
						dispute_info.candidate_hash.0);
					continue
				}

				let session_index = dispute_info.session;
				let session_info = self.get_session_keys(session_index).await;
				// TODO: we would like to distinguish different dispute phases at some point
				let voted_for = dispute_info
					.statements
					.iter()
					.filter(|(statement, _, _)| matches!(statement, DisputeStatement::Valid(_)))
					.count() as u32;
				let voted_against = dispute_info.statements.len() as u32 - voted_for;

				// This is a tracked outcome
				let outcome = stored_dispute.outcome.expect("checked above; qed");

				let misbehaving_validators = if outcome == SubxtDisputeResult::Valid {
					dispute_info
						.statements
						.iter()
						.filter(|(statement, _, _)| !matches!(statement, DisputeStatement::Valid(_)))
						.map(|(_, idx, _)| extract_validator_address(session_info.as_ref(), idx.0))
						.collect()
				} else {
					dispute_info
						.statements
						.iter()
						.filter(|(statement, _, _)| matches!(statement, DisputeStatement::Valid(_)))
						.map(|(_, idx, _)| extract_validator_address(session_info.as_ref(), idx.0))
						.collect()
				};
				self.disputes.push(DisputesTracker {
					candidate: dispute_info.candidate_hash.0,
					voted_for,
					voted_against,
					outcome,
					misbehaving_validators,
					resolve_time: Some(
						stored_dispute
							.concluded
							.expect("dispute must be concluded")
							.saturating_sub(stored_dispute.initiated),
					),
				});
			} else {
				// Not relevant dispute
				continue
			}
		}
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
				let bit = bitfield
					.0
					.as_bits()
					.get(core as usize)
					.expect("core index must be in the bitfield");
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
			self.relay_forks.last_mut().expect("must have relay fork").included_candidate =
				Some(self.current_candidate.candidate_hash.expect("must have candidate"));
		}
	}

	/// Called to move to idle state after inclusion/timeout.
	pub fn maybe_reset_state(&mut self) {
		if self.current_candidate.state == ParachainBlockState::Included {
			self.current_candidate.state = ParachainBlockState::Idle;
			self.current_candidate.candidate = None;
			self.current_candidate.candidate_hash = None;
		}
		self.disputes.clear();
		self.update = None;
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
			if let Some(candidate_hash) = self.current_candidate.candidate_hash {
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
