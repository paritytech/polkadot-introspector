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
use crate::{
	disputes_tracker::DisputesTracker,
	fork_tracker::ForkTracker,
	message_queus_tracker::MessageQueuesTracker,
	parachain_block_info::ParachainBlockInfo,
	progress::{ParachainConsensusEvent, ParachainProgressUpdate},
	prometheus::Metrics,
	stats::ParachainStats,
	tracker_rpc::TrackerRpc,
	tracker_storage::TrackerStorage,
	utils::{
		backed_candidate, extract_availability_bits_count, extract_inherent_fields, extract_misbehaving_validators,
		extract_validator_addresses, extract_votes, time_diff,
	},
};
use log::{error, info};
use polkadot_introspector_essentials::{
	api::subxt_wrapper::{RequestExecutor, SubxtWrapperError},
	chain_events::SubxtDisputeResult,
	collector::{CollectorStorageApi, DisputeInfo},
	metadata::polkadot_primitives,
	types::{BlockNumber, CoreOccupied, OnDemandOrder, Timestamp, H256},
};
use std::{default::Default, time::Duration};
use subxt::{
	config::{substrate::BlakeTwo256, Hasher},
	error::{Error, MetadataError},
};

/// A subxt based parachain candidate tracker.
pub struct SubxtTracker {
	/// Parachain ID to track.
	para_id: u32,
	/// A subxt API wrapper.
	rpc: TrackerRpc,
	/// API to access collector's storage
	storage: TrackerStorage,
	/// Observed new session
	new_session: Option<u32>,
	/// Information about current block we track.
	current_candidate: ParachainBlockInfo,
	/// Current relay chain block.
	current_relay_block: Option<(BlockNumber, H256)>,
	/// Previous relay chain block
	previous_relay_block: Option<(BlockNumber, H256)>,
	/// Current relay chain block timestamp.
	current_relay_block_ts: Option<Timestamp>,
	/// Current on-demand order
	on_demand_order: Option<OnDemandOrder>,
	/// Relay block where the last on-demand order was placed
	on_demand_order_block_number: Option<BlockNumber>,
	/// Timestamp where the last on-demand order was placed
	on_demand_order_ts: Option<Timestamp>,
	/// On-demand parachain was scheduled in current relay block
	is_on_demand_scheduled: bool,
	/// Last observed finality lag
	finality_lag: Option<u32>,
	/// Last relay chain block timestamp.
	last_relay_block_ts: Option<Timestamp>,
	/// Last included candidate in relay parent number
	last_included_at_block_number: Option<BlockNumber>,
	/// The relay chain timestamp at which the last candidate was included at.
	last_included_at_ts: Option<Timestamp>,
	/// Previous included candidate in relay parent number
	previous_included_at_block_number: Option<BlockNumber>,
	/// The relay chain timestamp at which the previous candidate was included at.
	previous_included_at_ts: Option<Timestamp>,
	/// The relay chain block number at which the last candidate was backed at.
	last_backed_at_block_number: Option<BlockNumber>,
	/// Disputes information if any disputes are there.
	disputes: Vec<DisputesTracker>,
	/// Messages queues status
	message_queues: MessageQueuesTracker,
	/// Current forks recorded
	relay_forks: Vec<ForkTracker>,
}

impl SubxtTracker {
	pub fn new(para_id: u32, node_rpc_url: &str, executor: RequestExecutor, api: CollectorStorageApi) -> Self {
		Self {
			para_id,
			rpc: TrackerRpc::new(para_id, node_rpc_url, executor),
			storage: TrackerStorage::new(para_id, api),
			current_candidate: Default::default(),
			new_session: None,
			current_relay_block: None,
			previous_relay_block: None,
			current_relay_block_ts: None,
			on_demand_order: None,
			on_demand_order_block_number: None,
			on_demand_order_ts: None,
			is_on_demand_scheduled: false,
			finality_lag: None,
			disputes: Vec::new(),
			last_backed_at_block_number: None,
			last_relay_block_ts: None,
			last_included_at_block_number: None,
			last_included_at_ts: None,
			previous_included_at_block_number: None,
			previous_included_at_ts: None,
			message_queues: Default::default(),
			relay_forks: vec![],
		}
	}

	pub async fn process_new_head(
		&mut self,
		block_hash: H256,
		block_number: BlockNumber,
		stats: &mut ParachainStats,
		metrics: &Metrics,
	) -> color_eyre::Result<Option<ParachainProgressUpdate>> {
		self.inject_block(block_hash, block_number).await?;
		let progress = self.progress(stats, metrics).await;
		self.maybe_reset_state();

		Ok(progress)
	}

	/// Called when a new session is observed
	pub async fn process_new_session(&mut self, session_index: u32) {
		self.new_session = Some(session_index)
	}

	/// Injects a new relay chain block into the tracker.
	/// Blocks must be injected in order.
	async fn inject_block(&mut self, block_hash: H256, block_number: BlockNumber) -> color_eyre::Result<()> {
		if let Some(inherent) = self.storage.inherent_data(block_hash).await {
			let (bitfields, backed_candidates, disputes) = extract_inherent_fields(inherent);

			self.set_relay_block(block_hash, block_number).await?;
			self.set_forks(block_hash, block_number);

			self.set_current_candidate(backed_candidates, bitfields.len(), block_number);
			self.set_core_assignment(block_hash).await?;
			self.set_disputes(&disputes[..]).await;

			self.set_hrmp_channels(block_hash).await?;
			self.set_on_demand_order(block_hash).await;

			// If a candidate was backed in this relay block, we don't need to process availability now.
			if self.has_backed_candidate() && !self.current_candidate.is_just_backed() {
				self.set_availability(block_hash, bitfields).await?;
			}
		} else {
			error!("Failed to get inherent data for {:?}", block_hash);
		}

		Ok(())
	}

	/// Update current parachain progress.
	async fn progress(&self, stats: &mut ParachainStats, metrics: &Metrics) -> Option<ParachainProgressUpdate> {
		if let Some((block_number, block_hash)) = self.current_relay_block {
			let timestamp = self.current_relay_block_ts.unwrap_or_default();
			let prev_timestamp = self.last_relay_block_ts.unwrap_or(timestamp);
			let mut progress = ParachainProgressUpdate {
				timestamp,
				prev_timestamp,
				block_number,
				block_hash,
				para_id: self.para_id,
				is_fork: self.is_fork(),
				finality_lag: self.finality_lag,
				core_occupied: self.current_candidate.core_occupied,
				..Default::default()
			};

			self.notify_new_session(&mut progress);
			self.notify_core_assignment(&mut progress);
			self.notify_bitfield_propagation(&mut progress, stats, metrics);
			self.notify_candidate_state(&mut progress, stats, metrics).await;
			self.notify_disputes(&mut progress, stats, metrics);
			self.notify_active_message_queues(&mut progress);
			self.notify_current_ts(stats, metrics);
			self.notify_finality_lag(metrics);
			self.notify_on_demand_order(metrics);

			Some(progress)
		} else {
			None
		}
	}

	/// Called to move to idle state after inclusion/timeout.
	fn maybe_reset_state(&mut self) {
		if self.current_candidate.is_backed() {
			self.on_demand_order_block_number = None;
			self.on_demand_order_ts = None;
		}
		self.new_session = None;
		self.on_demand_order = None;
		self.is_on_demand_scheduled = false;
		self.disputes.clear();
		self.current_candidate.maybe_reset();
	}

	async fn set_hrmp_channels(&mut self, block_hash: H256) -> color_eyre::Result<()> {
		let inbound = self.rpc.inbound_hrmp_channels(block_hash).await?;
		let outbound = self.rpc.outbound_hrmp_channels(block_hash).await?;
		self.message_queues.update_hrmp_channels(inbound, outbound);

		Ok(())
	}

	async fn set_relay_block(&mut self, block_hash: H256, block_number: BlockNumber) -> color_eyre::Result<()> {
		self.previous_relay_block = self.current_relay_block;
		self.current_relay_block = Some((block_number, block_hash));

		self.current_relay_block_ts = Some(self.rpc.block_timestamp(block_hash).await?);
		if !self.is_fork() {
			self.last_relay_block_ts = self.current_relay_block_ts;
		}

		self.finality_lag = self
			.storage
			.relevant_finalized_block_number(block_hash)
			.await
			.map(|num| block_number - num);

		Ok(())
	}

	async fn set_on_demand_order(&mut self, block_hash: H256) {
		self.on_demand_order = self.storage.on_demand_order(block_hash).await;
		if self.on_demand_order.is_some() {
			self.on_demand_order_block_number = self.current_relay_block.map(|(num, _)| num);
			self.on_demand_order_ts = self.current_relay_block_ts;
		}
	}

	fn set_forks(&mut self, block_hash: H256, block_number: BlockNumber) {
		if !self.is_fork() {
			self.relay_forks.clear();
		}
		self.relay_forks.push(ForkTracker {
			relay_hash: block_hash,
			relay_number: block_number,
			included_candidate: None,
			backed_candidate: None,
		});
	}

	fn set_current_candidate(
		&mut self,
		backed_candidates: Vec<polkadot_primitives::BackedCandidate<H256>>,
		bitfields_count: usize,
		block_number: BlockNumber,
	) {
		self.current_candidate.bitfield_count = bitfields_count as u32;
		// Update the curent state if a candiate was backed for this para.
		if let Some(candidate) = backed_candidate(backed_candidates, self.para_id) {
			self.current_candidate.set_just_backed();
			let commitments_hash = BlakeTwo256::hash_of(&candidate.candidate.commitments);
			let candidate_hash = BlakeTwo256::hash_of(&(&candidate.candidate.descriptor, commitments_hash));
			self.current_candidate.candidate_hash = Some(candidate_hash);
			self.current_candidate.candidate = Some(candidate);
			self.last_backed_at_block_number = Some(block_number);

			if let Some(current_fork) = self.relay_forks.last_mut() {
				current_fork.backed_candidate = Some(candidate_hash);
			}
		} else if !self.has_backed_candidate() {
			self.current_candidate.set_idle();
		}
	}

	async fn set_core_assignment(&mut self, block_hash: H256) -> color_eyre::Result<()> {
		// After adding On-demand Parachains, `ParaScheduler.Scheduled` API call will be removed
		let assignments = match self.rpc.core_assignments_via_scheduled_paras(block_hash).await {
			Ok(v) => v,
			// The `ParaScheduler,Scheduled` API call not found,
			// we should try to fetch `ParaScheduler,ClaimQueue` instead
			Err(SubxtWrapperError::SubxtError(Error::Metadata(MetadataError::StorageEntryNotFound(_)))) =>
				self.rpc.core_assignments_via_claim_queue(block_hash).await?,
			Err(e) => return Err(e.into()),
		};
		if let Some((&core, scheduled_ids)) = assignments.iter().find(|(_, ids)| ids.contains(&self.para_id)) {
			self.current_candidate.assigned_core = Some(core);
			self.current_candidate.core_occupied =
				matches!(self.rpc.occupied_cores(block_hash).await?[core as usize], CoreOccupied::Paras);
			self.is_on_demand_scheduled = self.on_demand_order.is_some() && scheduled_ids[0] == self.para_id;
		}
		Ok(())
	}

	async fn set_disputes(&mut self, disputes: &[polkadot_primitives::DisputeStatementSet]) {
		self.disputes = Vec::with_capacity(disputes.len());
		for dispute_info in disputes {
			if let Some(DisputeInfo { outcome, session_index, initiated, initiator_indices, concluded, .. }) =
				self.storage.dispute(dispute_info.candidate_hash.0).await
			{
				if let Some(outcome) = outcome {
					let session_info = self.storage.session_keys(dispute_info.session).await;
					// TODO: we would like to distinguish different dispute phases at some point
					let (voted_for, voted_against) = extract_votes(&dispute_info);
					let misbehaving_validators = extract_misbehaving_validators(
						session_info.as_ref(),
						dispute_info,
						outcome == SubxtDisputeResult::Valid,
					);
					let initiators_session_info = self.storage.session_keys(session_index).await;
					let initiators = extract_validator_addresses(initiators_session_info.as_ref(), initiator_indices);
					self.disputes.push(DisputesTracker {
						candidate: dispute_info.candidate_hash.0,
						voted_for,
						voted_against,
						outcome,
						initiators,
						misbehaving_validators,
						resolve_time: Some(concluded.expect("dispute must be concluded").saturating_sub(initiated)),
					});
				} else {
					info!(
						"dispute for candidate {} has been seen in the block inherent but is not tracked to be resolved",
						dispute_info.candidate_hash.0
					);
				}
			}
		}
	}

	async fn set_availability(
		&mut self,
		block_hash: H256,
		bitfields: Vec<polkadot_primitives::AvailabilityBitfield>,
	) -> color_eyre::Result<()> {
		if self.current_candidate.is_backed() {
			// We only process availability if our parachain is assigned to an availability core.
			if let Some(core) = self.current_candidate.assigned_core {
				self.current_candidate.current_availability_bits = extract_availability_bits_count(bitfields, core);
				self.current_candidate.max_availability_bits = self.validators_indices(block_hash).await?.len() as u32;

				if self.current_candidate.is_data_available() {
					self.current_candidate.set_included();
					self.relay_forks.last_mut().expect("must have relay fork").included_candidate =
						self.current_candidate.candidate_hash;
					self.previous_included_at_block_number = self.last_included_at_block_number;
					self.previous_included_at_ts = self.last_included_at_ts;
					self.last_included_at_block_number = self.current_relay_block.map(|v| v.0);
					self.last_included_at_ts = self.current_relay_block_ts;
				} else {
					self.current_candidate.set_pending();
				}
			}
		}

		Ok(())
	}

	fn notify_disputes(&self, progress: &mut ParachainProgressUpdate, stats: &mut ParachainStats, metrics: &Metrics) {
		self.disputes.iter().for_each(|outcome| {
			progress.events.push(ParachainConsensusEvent::Disputed(outcome.clone()));
			stats.on_disputed(outcome);
			metrics.on_disputed(outcome, self.para_id);
		});
	}

	fn notify_active_message_queues(&self, progress: &mut ParachainProgressUpdate) {
		if self.message_queues.has_hrmp_messages() {
			progress.events.push(ParachainConsensusEvent::MessageQueues(
				self.message_queues.active_inbound_channels(),
				self.message_queues.active_outbound_channels(),
			))
		}
	}

	fn notify_current_ts(&self, stats: &mut ParachainStats, metrics: &Metrics) {
		if !self.is_fork() {
			let ts = self.current_ts();
			stats.on_block(ts);
			metrics.on_block(ts.as_secs_f64(), self.para_id);
		}
	}

	fn notify_finality_lag(&self, metrics: &Metrics) {
		if let Some(finality_lag) = self.finality_lag {
			metrics.on_finality_lag(finality_lag);
		}
	}

	fn notify_on_demand_order(&self, metrics: &Metrics) {
		if let Some(ref order) = self.on_demand_order {
			metrics.handle_on_demand_order(order);
		}
		if let Some(delay) = self.on_demand_delay() {
			if self.is_on_demand_scheduled {
				metrics.handle_on_demand_delay(delay, self.para_id, "scheduled");
			}
			if self.current_candidate.is_backed() {
				metrics.handle_on_demand_delay(delay, self.para_id, "backed");
			}
		}
		if let Some(delay_sec) = self.on_demand_delay_ts() {
			if self.is_on_demand_scheduled {
				metrics.handle_on_demand_delay_sec(delay_sec, self.para_id, "scheduled");
			}
			if self.current_candidate.is_backed() {
				metrics.handle_on_demand_delay_sec(delay_sec, self.para_id, "backed");
			}
		}
	}

	fn notify_new_session(&self, progress: &mut ParachainProgressUpdate) {
		if let Some(session_index) = self.new_session {
			progress.events.push(ParachainConsensusEvent::NewSession(session_index));
		}
	}

	fn notify_core_assignment(&self, progress: &mut ParachainProgressUpdate) {
		if let Some(assigned_core) = self.current_candidate.assigned_core {
			progress.events.push(ParachainConsensusEvent::CoreAssigned(assigned_core));
		}
	}

	fn notify_bitfield_propagation(
		&self,
		progress: &mut ParachainProgressUpdate,
		stats: &mut ParachainStats,
		metrics: &Metrics,
	) {
		if self.current_candidate.is_bitfield_propagation_low() {
			progress.events.push(ParachainConsensusEvent::SlowBitfieldPropagation(
				self.current_candidate.bitfield_count,
				self.current_candidate.max_availability_bits,
			))
		}
		stats.on_bitfields(self.current_candidate.bitfield_count, self.current_candidate.is_bitfield_propagation_low());
		metrics.on_bitfields(
			self.current_candidate.bitfield_count,
			self.current_candidate.is_bitfield_propagation_low(),
			self.para_id,
		);
	}

	async fn notify_candidate_state(
		&self,
		progress: &mut ParachainProgressUpdate,
		stats: &mut ParachainStats,
		metrics: &Metrics,
	) {
		if self.current_candidate.is_idle() {
			progress.events.push(ParachainConsensusEvent::SkippedSlot);
			stats.on_skipped_slot(&progress);
			metrics.on_skipped_slot(&progress);
		}

		if self.current_candidate.is_backed() {
			if let Some(candidate_hash) = self.current_candidate.candidate_hash {
				progress.events.push(ParachainConsensusEvent::Backed(candidate_hash));
				stats.on_backed();
				metrics.on_backed(self.para_id);
			}
		}

		if self.current_candidate.is_pending() || self.current_candidate.is_included() {
			progress.bitfield_health.max_bitfield_count = self.current_candidate.max_availability_bits;
			progress.bitfield_health.available_count = self.current_candidate.current_availability_bits;
			progress.bitfield_health.bitfield_count = self.current_candidate.bitfield_count;

			if self.current_candidate.is_data_available() {
				if let Some(candidate_hash) = self.current_candidate.candidate_hash {
					progress.events.push(ParachainConsensusEvent::Included(
						candidate_hash,
						self.current_candidate.current_availability_bits,
						self.current_candidate.max_availability_bits,
					));

					let backed_in = self.candidate_backed_in(candidate_hash).await;
					let (relay_block_number, _) = self.current_relay_block.expect("Checked by caller; qed");
					stats.on_included(relay_block_number, self.previous_included_at_block_number, backed_in);
					metrics.on_included(
						relay_block_number,
						self.previous_included_at_block_number,
						backed_in,
						time_diff(self.current_relay_block_ts, self.previous_included_at_ts),
						self.para_id,
					);
				}
			} else if self.is_slow_availability() {
				progress.events.push(ParachainConsensusEvent::SlowAvailability(
					self.current_candidate.current_availability_bits,
					self.current_candidate.max_availability_bits,
				));
				stats.on_slow_availability();
				metrics.on_slow_availability(self.para_id);
			}
		}
	}

	fn current_ts(&self) -> Duration {
		let cur_ts = self.current_relay_block_ts.unwrap_or_default();
		let base_ts = self.last_relay_block_ts.unwrap_or(cur_ts);
		Duration::from_millis(cur_ts).saturating_sub(Duration::from_millis(base_ts))
	}

	fn is_fork(&self) -> bool {
		match (self.current_relay_block, self.previous_relay_block) {
			(Some((current, _)), Some((previous, _))) => current == previous,
			_ => false,
		}
	}

	fn on_demand_delay(&self) -> Option<u32> {
		if let (Some(on_demand), Some((relay, _))) = (self.on_demand_order_block_number, self.current_relay_block) {
			Some(relay.saturating_sub(on_demand))
		} else {
			None
		}
	}

	fn on_demand_delay_ts(&self) -> Option<Duration> {
		time_diff(self.current_relay_block_ts, self.on_demand_order_ts)
	}

	fn has_backed_candidate(&self) -> bool {
		self.current_candidate.candidate.is_some() ||
			self.relay_forks
				.iter()
				.any(|fork| fork.backed_candidate.is_some() || fork.included_candidate.is_some())
	}

	fn is_slow_availability(&self) -> bool {
		self.current_candidate.core_occupied &&
			self.last_backed_at_block_number != self.current_relay_block.map(|v| v.0)
	}

	async fn validators_indices(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Vec<polkadot_primitives::ValidatorIndex>> {
		Ok(self.rpc.backing_groups(block_hash).await?.into_iter().flatten().collect())
	}

	async fn candidate_backed_in(&self, candidate_hash: H256) -> Option<u32> {
		self.storage.candidate(candidate_hash).await.map(|v| {
			v.candidate_inclusion
				.backed
				.saturating_sub(v.candidate_inclusion.relay_parent_number)
		})
	}
}
