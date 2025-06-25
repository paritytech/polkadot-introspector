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
	message_queues_tracker::MessageQueuesTracker,
	parachain_block_info::{ParachainBlockInfo, ParachainBlockState},
	prometheus::PrometheusMetrics,
	stats::Stats,
	tracker_storage::TrackerStorage,
	types::{Block, BlockWithoutHash, DisputesTracker, ForkTracker, ParachainConsensusEvent, ParachainProgressUpdate},
	utils::{extract_availability_bits_count, extract_inherent_fields, time_diff},
};
use log::{error, info};
use polkadot_introspector_essentials::{
	collector::{BackedCandidateInfo, DisputeInfo, NewHeadEvent},
	metadata::polkadot_primitives::{AvailabilityBitfield, DisputeStatementSet, ValidatorIndex},
	types::{BlockNumber, CoreOccupied, H256, OnDemandOrder, Timestamp},
};
use std::{collections::HashMap, default::Default, time::Duration};

/// A subxt based parachain candidate tracker.
pub struct SubxtTracker {
	/// Parachain ID to track.
	para_id: u32,

	/// A new session index.
	new_session: Option<u32>,
	/// Information about current parachain block we track.
	/// `None` is for skipped slots.
	candidates: HashMap<u32, Vec<Option<ParachainBlockInfo>>>,
	/// Core assignments for current para_id
	cores: HashMap<u32, Vec<u32>>,
	/// Current relay chain block.
	current_relay_block: Option<Block>,
	/// Previous relay chain block.
	previous_relay_block: Option<Block>,

	/// A timestamp of a current relay chain block which is not a fork.
	current_non_fork_relay_block_ts: Option<Timestamp>,
	/// A timestamp of a last relay chain block which is not a fork.
	last_non_fork_relay_block_ts: Option<Timestamp>,
	/// The relay chain block number at which the last candidate was backed.
	last_backed_at_block_number: Option<BlockNumber>,
	/// The relay chain block at which last candidate was included.
	last_included_at: Option<BlockWithoutHash>,
	/// The relay chain block at which previous candidate was included.
	previous_included_at: Option<BlockWithoutHash>,

	/// Last observed finality lag.
	finality_lag: Option<u32>,

	/// Current on-demand order.
	on_demand_order: Option<OnDemandOrder>,
	/// Yhe relay chain block at which last on-demand order was placed.
	on_demand_order_at: Option<BlockWithoutHash>,
	/// On-demand parachain was scheduled in current relay block.
	is_on_demand_scheduled_in_current_block: bool,

	/// Disputes information.
	disputes: Vec<DisputesTracker>,
	/// Messages queues
	message_queues: MessageQueuesTracker,
	/// Current forks
	relay_forks: Vec<ForkTracker>,
}

impl SubxtTracker {
	pub fn new(para_id: u32) -> Self {
		Self {
			para_id,
			candidates: HashMap::new(),
			cores: HashMap::new(),
			new_session: None,
			current_relay_block: None,
			previous_relay_block: None,
			on_demand_order: None,
			on_demand_order_at: None,
			is_on_demand_scheduled_in_current_block: false,
			finality_lag: None,
			disputes: Vec::new(),
			last_backed_at_block_number: None,
			current_non_fork_relay_block_ts: None,
			last_non_fork_relay_block_ts: None,
			last_included_at: None,
			previous_included_at: None,
			message_queues: Default::default(),
			relay_forks: vec![],
		}
	}

	/// Saves new session to tracker's state
	pub fn inject_new_session(&mut self, session_index: u32) {
		self.new_session = Some(session_index)
	}

	/// Injects a new relay chain block into the tracker. Blocks must be injected in order.
	pub async fn inject_block(
		&mut self,
		block_hash: H256,
		new_head: NewHeadEvent,
		storage: &TrackerStorage,
	) -> color_eyre::Result<()> {
		if let Some(inherent) = storage.inherent_data(block_hash).await {
			let (bitfields, disputes) = extract_inherent_fields(inherent);
			let block_number = new_head.relay_parent_number;

			self.set_relay_block(block_hash, block_number, storage).await?;
			self.set_forks(block_hash, block_number);
			self.set_cores(block_hash, storage).await;
			self.set_included_candidates(new_head.candidates_included.as_slice()).await;
			self.set_backed_candidates(new_head.candidates_backed.as_slice(), bitfields.len(), block_number)
				.await;
			self.set_core_assignment(block_hash, storage).await;
			self.set_disputes(disputes.as_slice(), new_head.disputes_concluded.as_slice(), storage)
				.await;
			self.set_hrmp_channels(block_hash, storage).await?;
			self.set_on_demand_order(block_hash, storage).await;
			self.set_pending_availability(block_hash, bitfields, storage).await?;
			self.set_dropped_candidates();
		} else {
			error!("Failed to get inherent data for {:?}", block_hash);
		}

		Ok(())
	}

	/// Creates a parachain progress.
	pub async fn progress(
		&self,
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
		storage: &TrackerStorage,
	) -> Option<ParachainProgressUpdate> {
		if let Some(block) = self.current_relay_block {
			let mut progress = ParachainProgressUpdate {
				timestamp: block.ts,
				prev_timestamp: self.last_non_fork_relay_block_ts.unwrap_or(block.ts),
				block_number: block.num,
				block_hash: block.hash,
				para_id: self.para_id,
				is_fork: self.is_fork(),
				finality_lag: self.finality_lag,
				core_occupied: self
					.candidates
					.iter()
					.map(|(core_idx, v)| {
						(*core_idx, v.last().is_some_and(|v| v.as_ref().is_some_and(|v| v.core_occupied)))
					})
					.collect(),
				..Default::default()
			};

			self.notify_new_session(&mut progress);
			self.notify_core_assignment(&mut progress);
			self.notify_bitfield_propagation(&mut progress, stats, metrics);
			self.notify_candidate_state(&mut progress, stats, metrics, storage).await;
			self.notify_disputes(&mut progress, stats, metrics);
			self.notify_active_message_queues(&mut progress);
			self.notify_current_block_time(stats, metrics);
			self.notify_finality_lag(metrics);
			self.notify_on_demand_order(metrics);

			Some(progress)
		} else {
			None
		}
	}

	/// Resets state
	pub fn maybe_reset_state(&mut self) {
		for &core in self.cores.keys() {
			if self.is_current_candidate_backed(core) {
				self.on_demand_order_at = None;
			}
		}
		self.new_session = None;
		self.on_demand_order = None;
		self.is_on_demand_scheduled_in_current_block = false;
		self.disputes.clear();
		self.candidates.values_mut().for_each(|v| {
			v.retain(|v| {
				v.is_some() &&
					v.as_ref()
						.is_some_and(|candidate| !candidate.is_included() && !candidate.is_dropped())
			})
		});
	}

	async fn set_hrmp_channels(&mut self, block_hash: H256, storage: &TrackerStorage) -> color_eyre::Result<()> {
		let (inbound, outbound) = storage.inbound_outbound_hrmp_channels(block_hash).await.unwrap_or_default();
		self.message_queues.set_hrmp_channels(inbound, outbound);

		Ok(())
	}

	async fn set_relay_block(
		&mut self,
		block_hash: H256,
		block_number: BlockNumber,
		storage: &TrackerStorage,
	) -> color_eyre::Result<()> {
		let ts = storage.block_timestamp(block_hash).await.expect("saved in the collector");
		self.previous_relay_block = self.current_relay_block;
		self.current_relay_block = Some(Block { num: block_number, ts, hash: block_hash });

		if !self.is_fork() {
			self.last_non_fork_relay_block_ts = self.current_non_fork_relay_block_ts;
			self.current_non_fork_relay_block_ts = Some(ts);
		}

		self.finality_lag = storage
			.relevant_finalized_block_number(block_hash)
			.await
			.map(|num| block_number - num);

		Ok(())
	}

	async fn set_on_demand_order(&mut self, block_hash: H256, storage: &TrackerStorage) {
		self.on_demand_order = storage.on_demand_order(block_hash).await;
		if self.on_demand_order.is_some() {
			self.on_demand_order_at = self.current_relay_block.map(|v| v.into());
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

	async fn set_cores(&mut self, block_hash: H256, storage: &TrackerStorage) {
		let assignments = storage.core_assignments(block_hash).await.expect("saved in the collector");
		self.cores = assignments
			.iter()
			.filter_map(|(core, ids)| {
				let ids = ids.iter().map(|v| v.0).collect::<Vec<_>>();
				ids.contains(&self.para_id).then_some((core.0, ids))
			})
			.collect();
	}

	async fn set_included_candidates(&mut self, candidates_included: &[H256]) {
		let mut last_included = None;
		for candidate in self.all_candidates_mut() {
			if candidates_included.contains(&candidate.candidate_hash) {
				candidate.set_included();
				last_included = Some(candidate.candidate_hash);
			}
		}
		if last_included.is_some() {
			self.relay_forks.last_mut().expect("must have relay fork").included_candidate = last_included;
			self.previous_included_at = self.last_included_at;
			self.last_included_at = self.current_relay_block.map(|v| v.into());
		}
	}

	async fn set_backed_candidates(
		&mut self,
		backed_candidates: &[BackedCandidateInfo],
		bitfields_count: usize,
		block_number: BlockNumber,
	) {
		let mut used_cores = vec![];
		for candidate in backed_candidates.iter().filter(|v| v.para_id == self.para_id) {
			let core = candidate.core_idx;
			let candidate = ParachainBlockInfo::new(candidate.candidate_hash, core, bitfields_count as u32);
			if let Some(current_fork) = self.relay_forks.last_mut() {
				current_fork.backed_candidate = Some(candidate.candidate_hash);
			}
			self.last_backed_at_block_number = Some(block_number);
			self.candidates.entry(core).or_default().push(Some(candidate));

			used_cores.push(core)
		}

		let idle_cores = self.cores.keys().filter(|v| !used_cores.contains(v)).cloned();
		for core in idle_cores {
			if !self.has_backed_candidate(core) {
				self.candidates.entry(core).or_default().push(None);
			}
		}
	}

	fn set_dropped_candidates(&mut self) {
		for candidates in self.candidates.values_mut() {
			let mut backed_candidates: Vec<_> = candidates
				.iter_mut()
				.flatten()
				.filter(|candidate| !candidate.is_included())
				.collect();
			// We have more than one backed candidate
			// because of session change or availability core timeout
			if backed_candidates.len() > 1 {
				// We keep tracking the last backed candidate
				let _ = backed_candidates.pop();
				for candidate in backed_candidates {
					candidate.set_dropped();
				}
			}
		}
	}

	async fn set_core_assignment(&mut self, block_hash: H256, storage: &TrackerStorage) {
		for (core, scheduled_ids) in self.cores.clone() {
			self.is_on_demand_scheduled_in_current_block =
				self.on_demand_order.is_some() && scheduled_ids[0] == self.para_id;

			let Some(candidate) = self.current_candidate_mut(core) else { continue };
			if candidate.is_backed() {
				candidate.core_occupied = matches!(
					storage.occupied_cores(block_hash).await.expect("saved in the collector")[core as usize],
					CoreOccupied::Occupied
				);
			}
		}
	}

	/// Set disputes
	///
	/// We track disputes based on the DisputeConcluded event
	/// because disputes in inherent data can persist even after they are concluded,
	/// leading to inaccurate metrics.
	async fn set_disputes(
		&mut self,
		inherent_disputes: &[DisputeStatementSet],
		concluded_disputes: &[DisputeInfo],
		storage: &TrackerStorage,
	) {
		self.disputes = Vec::with_capacity(concluded_disputes.len());
		for concluded_dispute in concluded_disputes {
			let Some(dispute_info) = inherent_disputes
				.iter()
				.find(|&v| v.candidate_hash.0 == concluded_dispute.dispute.candidate_hash)
			else {
				log::warn!(
					"Concluded dispute appeared in events but is not present in inherent data: {:?}",
					concluded_dispute.dispute.candidate_hash
				);
				continue;
			};
			if let Some(DisputeInfo { outcome, session_index, initiated, initiator_indices, concluded, .. }) =
				storage.dispute(dispute_info.candidate_hash.0).await
			{
				if let Some(outcome) = outcome {
					self.disputes.push(DisputesTracker::new(
						dispute_info,
						outcome,
						initiated,
						initiator_indices,
						concluded.expect("dispute must be concluded"),
						storage.session_keys(dispute_info.session).await.as_ref(),
						storage.session_keys(session_index).await.as_ref(),
					));
				} else {
					info!(
						"parachain {}: dispute for candidate {} has been seen in the block inherent but is not tracked to be resolved",
						self.para_id, dispute_info.candidate_hash.0
					);
				}
			}
		}
	}

	async fn set_pending_availability(
		&mut self,
		block_hash: H256,
		bitfields: Vec<AvailabilityBitfield>,
		storage: &TrackerStorage,
	) -> color_eyre::Result<()> {
		let core_ids: Vec<u32> = self.cores.keys().cloned().collect();
		for core in core_ids {
			if self.is_current_candidate_backed(core) && !self.is_just_backed() {
				let max_bits = self.validators_indices(block_hash, storage).await?.len() as u32;
				let candidate = self.current_candidate_mut(core).expect("Checked above");

				candidate.current_availability_bits = extract_availability_bits_count(&bitfields, core);
				candidate.max_availability_bits = max_bits;
				candidate.set_pending();
			}
		}

		Ok(())
	}

	fn notify_disputes(
		&self,
		progress: &mut ParachainProgressUpdate,
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
	) {
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

	fn notify_current_block_time(&self, stats: &mut impl Stats, metrics: &impl PrometheusMetrics) {
		if !self.is_fork() {
			let ts = self.current_block_time();
			stats.on_block(ts);
			metrics.on_block(ts, self.para_id);
		}
	}

	fn notify_finality_lag(&self, metrics: &impl PrometheusMetrics) {
		if let Some(finality_lag) = self.finality_lag {
			metrics.on_finality_lag(finality_lag);
		}
	}

	fn notify_on_demand_order(&self, metrics: &impl PrometheusMetrics) {
		if let Some(ref order) = self.on_demand_order {
			metrics.handle_on_demand_order(order);
		}
		if let Some(delay) = self.on_demand_delay() {
			if self.is_on_demand_scheduled_in_current_block {
				metrics.handle_on_demand_delay(delay, self.para_id, "scheduled");
			}
			for &core in self.cores.keys() {
				if self.is_current_candidate_backed(core) {
					metrics.handle_on_demand_delay(delay, self.para_id, "backed");
				}
			}
		}
		if let Some(delay_sec) = self.on_demand_delay_sec() {
			if self.is_on_demand_scheduled_in_current_block {
				metrics.handle_on_demand_delay_sec(delay_sec, self.para_id, "scheduled");
			}
			for &core in self.cores.keys() {
				if self.is_current_candidate_backed(core) {
					metrics.handle_on_demand_delay_sec(delay_sec, self.para_id, "backed");
				}
			}
		}
	}

	fn notify_new_session(&self, progress: &mut ParachainProgressUpdate) {
		if let Some(session_index) = self.new_session {
			progress.events.push(ParachainConsensusEvent::NewSession(session_index));
		}
	}

	fn notify_core_assignment(&self, progress: &mut ParachainProgressUpdate) {
		for &core in self.cores.keys() {
			if self.current_candidate(core).is_some() {
				progress.events.push(ParachainConsensusEvent::CoreAssigned(core));
			}
		}
	}

	fn notify_bitfield_propagation(
		&self,
		progress: &mut ParachainProgressUpdate,
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
	) {
		for &core in self.cores.keys() {
			let Some(candidate) = self.current_candidate(core) else { continue };
			if candidate.is_bitfield_propagation_slow() {
				progress.events.push(ParachainConsensusEvent::SlowBitfieldPropagation(
					candidate.bitfield_count,
					candidate.max_availability_bits,
				))
			}
			stats.on_bitfields(candidate.bitfield_count, candidate.is_bitfield_propagation_slow());
			metrics.on_bitfields(candidate.bitfield_count, candidate.is_bitfield_propagation_slow(), self.para_id);
		}
	}

	async fn notify_candidate_state(
		&self,
		progress: &mut ParachainProgressUpdate,
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
		storage: &TrackerStorage,
	) {
		for candidate in self.all_candidates_and_skipped_slots() {
			// Process skipped slots first
			if candidate.is_none() {
				progress.events.push(ParachainConsensusEvent::SkippedSlot);
				stats.on_skipped_slot(progress);
				metrics.on_skipped_slot(progress);
			}

			let Some(candidate) = candidate else { continue };
			match candidate.state {
				ParachainBlockState::Backed => {
					progress.events.push(ParachainConsensusEvent::Backed(candidate.candidate_hash));
					stats.on_backed();
					metrics.on_backed(self.para_id);
				},
				ParachainBlockState::PendingAvailability => {
					progress
						.events
						.push(ParachainConsensusEvent::PendingAvailability(candidate.candidate_hash));
					if self.is_slow_availability(candidate.assigned_core) {
						progress.events.push(ParachainConsensusEvent::SlowAvailability(
							candidate.current_availability_bits,
							candidate.max_availability_bits,
						));
						stats.on_slow_availability();
						metrics.on_slow_availability(self.para_id);
					}
				},
				ParachainBlockState::Included => {
					progress.events.push(ParachainConsensusEvent::Included(
						candidate.candidate_hash,
						candidate.current_availability_bits,
						candidate.max_availability_bits,
					));

					let backed_in = self.candidate_backed_in(candidate.candidate_hash, storage).await;
					let relay_block = self.current_relay_block.expect("Checked by caller; qed");
					stats.on_included(relay_block.num, self.previous_included_at.map(|v| v.num), backed_in);
					metrics.on_included(
						relay_block.num,
						self.previous_included_at.map(|v| v.num),
						backed_in,
						time_diff(Some(relay_block.ts), self.previous_included_at.map(|v| v.ts)),
						self.para_id,
					);
				},
				ParachainBlockState::Dropped =>
					progress.events.push(ParachainConsensusEvent::Dropped(candidate.candidate_hash)),
			}
		}
	}

	fn current_block_time(&self) -> Duration {
		let cur_ts = self.current_relay_block.map(|v| v.ts).unwrap_or_default();
		let base_ts = self.last_non_fork_relay_block_ts.unwrap_or(cur_ts);
		Duration::from_millis(cur_ts).saturating_sub(Duration::from_millis(base_ts))
	}

	fn current_candidate(&self, core: u32) -> Option<&ParachainBlockInfo> {
		self.candidates.get(&core).and_then(|v| v.last()).and_then(|v| v.as_ref())
	}

	fn current_candidate_mut(&mut self, core: u32) -> Option<&mut ParachainBlockInfo> {
		self.candidates
			.get_mut(&core)
			.and_then(|v| v.last_mut())
			.and_then(|v| v.as_mut())
	}

	fn all_candidates_and_skipped_slots(&self) -> impl Iterator<Item = Option<&ParachainBlockInfo>> {
		self.candidates.values().flatten().map(|v| v.as_ref())
	}

	fn all_candidates_mut(&mut self) -> impl Iterator<Item = &mut ParachainBlockInfo> {
		self.candidates.values_mut().flatten().filter_map(|v| v.as_mut())
	}

	fn is_fork(&self) -> bool {
		match (self.current_relay_block, self.previous_relay_block) {
			(Some(a), Some(b)) => a.num == b.num,
			_ => false,
		}
	}

	fn on_demand_delay(&self) -> Option<u32> {
		if let (Some(on_demand), Some(relay)) = (self.on_demand_order_at, self.current_relay_block) {
			Some(relay.num.saturating_sub(on_demand.num))
		} else {
			None
		}
	}

	fn on_demand_delay_sec(&self) -> Option<Duration> {
		time_diff(self.current_relay_block.map(|v| v.ts), self.on_demand_order_at.map(|v| v.ts))
	}

	fn has_backed_candidate(&self, core: u32) -> bool {
		self.candidates
			.get(&core)
			.is_some_and(|v| v.iter().any(|candidate| candidate.is_some())) ||
			self.relay_forks
				.iter()
				.any(|fork| fork.backed_candidate.is_some() || fork.included_candidate.is_some())
	}

	fn is_current_candidate_backed(&self, core: u32) -> bool {
		self.current_candidate(core).is_some_and(|v| v.is_backed())
	}

	fn is_just_backed(&self) -> bool {
		self.last_backed_at_block_number.is_some() &&
			self.last_backed_at_block_number == self.current_relay_block.map(|v| v.num)
	}

	fn is_slow_availability(&self, core: u32) -> bool {
		self.current_candidate(core).is_some_and(|v| v.core_occupied) && !self.is_just_backed()
	}

	async fn validators_indices(
		&self,
		block_hash: H256,
		storage: &TrackerStorage,
	) -> color_eyre::Result<Vec<ValidatorIndex>> {
		Ok(storage
			.backing_groups(block_hash)
			.await
			.expect("saved in the collector")
			.into_iter()
			.flatten()
			.collect())
	}

	async fn candidate_backed_in(&self, candidate_hash: H256, storage: &TrackerStorage) -> Option<u32> {
		storage.candidate(candidate_hash).await.map(|v| {
			v.candidate_inclusion
				.backed
				.saturating_sub(v.candidate_inclusion.relay_parent_number)
		})
	}
}

#[cfg(test)]
mod test_inject_new_session {
	use super::*;

	#[tokio::test]
	async fn test_sets_new_session() {
		let mut tracker = SubxtTracker::new(100);
		assert!(tracker.new_session.is_none());

		tracker.inject_new_session(42);

		assert_eq!(tracker.new_session, Some(42));
	}
}

#[cfg(test)]
mod test_maybe_reset_state {
	use super::*;
	use crate::test_utils::{create_hasher, create_para_block_info};

	#[tokio::test]
	async fn test_resets_state_if_not_backed() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let mut candidate = create_para_block_info(100, hasher);
		candidate.set_included();
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.new_session = Some(42);
		tracker.on_demand_order = Some(OnDemandOrder::default());
		tracker.is_on_demand_scheduled_in_current_block = true;
		tracker.disputes = vec![DisputesTracker::default()];
		tracker.cores.entry(0).or_default().push(100);

		tracker.maybe_reset_state();

		assert!(tracker.new_session.is_none());
		assert!(tracker.on_demand_order.is_none());
		assert!(!tracker.is_on_demand_scheduled_in_current_block);
		assert!(tracker.disputes.is_empty());
		for (_, candidates) in tracker.candidates {
			assert!(candidates.is_empty());
		}
	}

	#[tokio::test]
	async fn test_resets_state_if_backed() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let candidate = create_para_block_info(100, hasher);
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.new_session = Some(42);
		tracker.on_demand_order = Some(OnDemandOrder::default());
		tracker.on_demand_order_at = Some(BlockWithoutHash::default());
		tracker.is_on_demand_scheduled_in_current_block = true;
		tracker.disputes = vec![DisputesTracker::default()];
		tracker.cores.entry(0).or_default().push(100);

		assert!(tracker.is_current_candidate_backed(0));
		tracker.maybe_reset_state();

		assert!(tracker.on_demand_order_at.is_none());
		assert!(tracker.new_session.is_none());
		assert!(tracker.on_demand_order.is_none());
		assert!(!tracker.is_on_demand_scheduled_in_current_block);
		assert!(tracker.disputes.is_empty());
	}
}

#[cfg(test)]
mod test_inject_block {
	use super::*;
	use crate::test_utils::{
		candidate_hash, create_candidate_record, create_hasher, create_inherent_data, create_para_block_info,
		create_storage, storage_write,
	};
	use polkadot_introspector_essentials::collector::CollectorPrefixType;
	use std::collections::BTreeMap;

	#[tokio::test]
	async fn test_changes_nothing_if_there_is_no_inherent_data() {
		let hash = H256::random();
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);

		tracker
			.inject_block(hash, NewHeadEvent::with_relay_parent_number(0), &tracker_storage)
			.await
			.unwrap();

		assert!(tracker.new_session.is_none());
		assert!(tracker.candidates.is_empty());
		assert!(tracker.current_relay_block.is_none());
		assert!(tracker.previous_relay_block.is_none());
		assert!(tracker.last_non_fork_relay_block_ts.is_none());
		assert!(tracker.last_backed_at_block_number.is_none());
		assert!(tracker.last_included_at.is_none());
		assert!(tracker.previous_included_at.is_none());
		assert!(tracker.finality_lag.is_none());
		assert!(tracker.on_demand_order.is_none());
		assert!(tracker.on_demand_order_at.is_none());
		assert!(!tracker.is_on_demand_scheduled_in_current_block);
		assert!(tracker.disputes.is_empty());
		assert!(!tracker.message_queues.has_hrmp_messages());
		assert!(tracker.relay_forks.is_empty());
	}

	#[tokio::test]
	async fn test_sets_relay_block() {
		let first_hash = H256::random();
		let second_hash = H256::random();
		let storage = create_storage().await;
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, storage.clone(), hasher);

		// Inject a block
		storage_write(
			CollectorPrefixType::CoreAssignments,
			first_hash,
			BTreeMap::<u32, Vec<u32>>::from([(0, vec![100])]),
			&storage,
		)
		.await
		.unwrap();
		storage_write(CollectorPrefixType::BackingGroups, first_hash, Vec::<Vec<ValidatorIndex>>::default(), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::InherentData, first_hash, create_inherent_data(100), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::Timestamp, first_hash, 1_u64, &storage)
			.await
			.unwrap();
		tracker
			.inject_block(first_hash, NewHeadEvent::with_relay_parent_number(42), &tracker_storage)
			.await
			.unwrap();

		let current = tracker.current_relay_block.unwrap();
		assert!(tracker.previous_relay_block.is_none());
		assert_eq!(current.hash, first_hash);
		assert_eq!(tracker.current_non_fork_relay_block_ts, Some(1_u64));
		assert_eq!(tracker.last_non_fork_relay_block_ts, None);
		assert!(tracker.finality_lag.is_none());

		// Inject a fork and relevant finalized block number
		storage_write(
			CollectorPrefixType::CoreAssignments,
			second_hash,
			BTreeMap::<u32, Vec<u32>>::from([(0, vec![100])]),
			&storage,
		)
		.await
		.unwrap();
		storage_write(CollectorPrefixType::BackingGroups, second_hash, Vec::<Vec<ValidatorIndex>>::default(), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::InherentData, second_hash, create_inherent_data(100), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::RelevantFinalizedBlockNumber, second_hash, 40, &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::Timestamp, second_hash, 2_u64, &storage)
			.await
			.unwrap();
		tracker
			.inject_block(second_hash, NewHeadEvent::with_relay_parent_number(42), &tracker_storage)
			.await
			.unwrap();

		let previous = tracker.previous_relay_block.unwrap();
		let current = tracker.current_relay_block.unwrap();
		assert_eq!(previous.hash, first_hash);
		assert_eq!(current.hash, second_hash);
		assert_eq!(tracker.current_non_fork_relay_block_ts, Some(1));
		assert_eq!(tracker.last_non_fork_relay_block_ts, None);
		assert_eq!(tracker.finality_lag, Some(2));
	}

	#[tokio::test]
	async fn test_sets_backed_candidates() {
		let storage = create_storage().await;
		let hasher = create_hasher().await;
		let tracker_storage = TrackerStorage::new(100, storage.clone(), hasher);
		let mut tracker = SubxtTracker::new(100);

		let block_hash = H256::random();
		let inherent_data = create_inherent_data(100);
		let backed_candidate = inherent_data.backed_candidates.first().unwrap();
		let candidate_hash = candidate_hash(backed_candidate, hasher);

		// Inject a block
		storage_write(
			CollectorPrefixType::CoreAssignments,
			block_hash,
			BTreeMap::<u32, Vec<u32>>::from([(0, vec![100])]),
			&storage,
		)
		.await
		.unwrap();
		storage_write(CollectorPrefixType::OccupiedCores, block_hash, vec![CoreOccupied::Scheduled], &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::BackingGroups, block_hash, Vec::<Vec<ValidatorIndex>>::default(), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::InherentData, block_hash, inherent_data, &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::Timestamp, block_hash, 1_u64, &storage)
			.await
			.unwrap();

		let mut head_event = NewHeadEvent::with_relay_parent_number(42);
		head_event
			.candidates_backed
			.push(BackedCandidateInfo { candidate_hash, core_idx: 0, para_id: 100 });
		tracker.inject_block(block_hash, head_event, &tracker_storage).await.unwrap();

		let candidate = tracker.candidates.get(&0).unwrap().first().unwrap().as_ref().unwrap();
		assert!(candidate.candidate_hash == candidate_hash);
		assert!(candidate.is_backed());
	}

	#[tokio::test]
	async fn test_sets_dropped_candidates() {
		let storage = create_storage().await;
		let hasher = create_hasher().await;
		let tracker_storage = TrackerStorage::new(100, storage.clone(), hasher);
		let mut tracker = SubxtTracker::new(100);

		let block_hash = H256::random();
		let inherent_data = create_inherent_data(100);
		let backed_candidate = inherent_data.backed_candidates.first().unwrap();
		let candidate_hash = candidate_hash(backed_candidate, hasher);

		// Inject a block
		storage_write(
			CollectorPrefixType::CoreAssignments,
			block_hash,
			BTreeMap::<u32, Vec<u32>>::from([(0, vec![100])]),
			&storage,
		)
		.await
		.unwrap();
		storage_write(CollectorPrefixType::OccupiedCores, block_hash, vec![CoreOccupied::Scheduled], &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::BackingGroups, block_hash, Vec::<Vec<ValidatorIndex>>::default(), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::InherentData, block_hash, inherent_data, &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::Timestamp, block_hash, 1_u64, &storage)
			.await
			.unwrap();
		storage_write(
			CollectorPrefixType::Candidate(100),
			candidate_hash,
			create_candidate_record(100, 42, None, H256::random(), 40),
			&storage,
		)
		.await
		.unwrap();

		// Actually, we inject the same block twice, but for current asserts it is OK
		let mut head_event_42 = NewHeadEvent::with_relay_parent_number(42);
		head_event_42
			.candidates_backed
			.push(BackedCandidateInfo { candidate_hash, core_idx: 0, para_id: 100 });
		tracker.inject_block(block_hash, head_event_42, &tracker_storage).await.unwrap();
		let mut head_event_43 = NewHeadEvent::with_relay_parent_number(42);
		head_event_43
			.candidates_backed
			.push(BackedCandidateInfo { candidate_hash, core_idx: 0, para_id: 100 });
		tracker.inject_block(block_hash, head_event_43, &tracker_storage).await.unwrap();

		let candidates = tracker.candidates.get(&0).unwrap();
		assert!(candidates.len() == 2);
		assert!(candidates.first().unwrap().as_ref().unwrap().is_dropped());
		assert!(candidates.get(1).unwrap().as_ref().unwrap().is_backed());
	}

	#[tokio::test]
	async fn test_sets_included_candidates() {
		let storage = create_storage().await;
		let hasher = create_hasher().await;
		let tracker_storage = TrackerStorage::new(100, storage.clone(), hasher);
		let mut tracker = SubxtTracker::new(100);

		let block_hash = H256::random();
		let candidate = create_para_block_info(100, hasher);
		let candidate_hash = candidate.candidate_hash;
		tracker.candidates.entry(0).or_default().push(Some(candidate));

		// Inject a block
		storage_write(
			CollectorPrefixType::CoreAssignments,
			block_hash,
			BTreeMap::<u32, Vec<u32>>::from([(0, vec![100])]),
			&storage,
		)
		.await
		.unwrap();
		storage_write(CollectorPrefixType::OccupiedCores, block_hash, vec![CoreOccupied::Free], &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::BackingGroups, block_hash, Vec::<Vec<ValidatorIndex>>::default(), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::InherentData, block_hash, create_inherent_data(100), &storage)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::Timestamp, block_hash, 1_u64, &storage)
			.await
			.unwrap();

		let mut head_event = NewHeadEvent::with_relay_parent_number(42);
		head_event.candidates_included.push(candidate_hash);
		tracker.inject_block(block_hash, head_event, &tracker_storage).await.unwrap();

		let candidate = tracker.candidates.get(&0).unwrap().first().unwrap().as_ref().unwrap();
		assert!(candidate.is_included());
	}
}

#[cfg(test)]
mod test_progress {
	use super::*;
	use crate::{
		prometheus::{Metrics, MockPrometheusMetrics},
		stats::{MockStats, ParachainStats},
		test_utils::{
			create_candidate_record, create_hasher, create_hrmp_channels, create_para_block_info, create_storage,
			storage_write,
		},
	};
	use mockall::predicate::eq;
	use polkadot_introspector_essentials::collector::CollectorPrefixType;

	#[tokio::test]
	async fn test_returns_none_if_no_current_block() {
		let hasher = create_hasher().await;
		let tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut stats = MockStats::default();
		let metrics = Metrics::default();

		let progress = tracker.progress(&mut stats, &metrics, &tracker_storage).await;

		assert!(progress.is_none());
	}

	#[tokio::test]
	async fn test_returns_progress_on_current_block() {
		let hash = H256::random();
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut stats = ParachainStats::default();
		let metrics = Metrics::default();

		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash });
		let progress = tracker.progress(&mut stats, &metrics, &tracker_storage).await.unwrap();

		assert_eq!(progress.timestamp, 1694095332000);
		assert_eq!(progress.prev_timestamp, 1694095332000);
		assert_eq!(progress.block_number, 42);
		assert_eq!(progress.block_hash, hash);
		assert_eq!(progress.para_id, 100);
		assert!(!progress.is_fork);
		assert!(progress.finality_lag.is_none());
		for (_, core_occupied) in progress.core_occupied {
			assert!(!core_occupied);
		}
	}

	#[tokio::test]
	async fn test_includes_new_session_if_exist() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut stats = ParachainStats::default();
		let metrics = Metrics::default();

		// No new session
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		let progress = tracker.progress(&mut stats, &metrics, &tracker_storage).await.unwrap();

		assert!(
			!progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::NewSession(_)))
		);

		// With new session
		tracker.inject_new_session(12);
		let progress = tracker.progress(&mut stats, &metrics, &tracker_storage).await.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::NewSession(_)))
		);
	}

	#[tokio::test]
	async fn test_includes_core_assignment() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut stats = ParachainStats::default();
		let metrics = Metrics::default();
		let mut candidate = create_para_block_info(100, hasher);

		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		candidate.core_occupied = true;
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.cores.entry(0).or_default().push(100);
		let progress = tracker.progress(&mut stats, &metrics, &tracker_storage).await.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::CoreAssigned(0)))
		);
	}

	#[tokio::test]
	async fn test_includes_slow_propogation() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut candidate = create_para_block_info(100, hasher);
		let mut mock_stats = MockStats::default();
		mock_stats.expect_on_backed().returning(|| ());
		mock_stats.expect_on_block().returning(|_| ());
		mock_stats.expect_on_skipped_slot().returning(|_| ());
		let mut mock_metrics = MockPrometheusMetrics::default();
		mock_metrics.expect_on_backed().returning(|_| ());
		mock_metrics.expect_on_block().returning(|_, _| ());
		mock_metrics.expect_on_skipped_slot().returning(|_| ());

		// Bitfields propogation isn't slow
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		candidate.bitfield_count = 120;
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.cores.entry(0).or_default().push(100);
		mock_stats.expect_on_bitfields().with(eq(120), eq(false)).returning(|_, _| ());
		mock_metrics
			.expect_on_bitfields()
			.with(eq(120), eq(false), eq(100))
			.returning(|_, _, _| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			!progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::SlowBitfieldPropagation(_, _)))
		);

		// Bitfields propogation is slow
		let candidate = tracker.candidates.entry(0).or_default().last_mut().unwrap().as_mut().unwrap();
		candidate.max_availability_bits = 200;
		mock_stats.expect_on_bitfields().with(eq(120), eq(true)).returning(|_, _| ());
		mock_metrics
			.expect_on_bitfields()
			.with(eq(120), eq(true), eq(100))
			.returning(|_, _, _| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::SlowBitfieldPropagation(_, _)))
		);
	}

	#[tokio::test]
	async fn test_includes_message_queues() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut stats = ParachainStats::default();
		let metrics = Metrics::default();

		// No active channels
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		let progress = tracker.progress(&mut stats, &metrics, &tracker_storage).await.unwrap();

		assert!(
			!progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::MessageQueues(_, _)))
		);

		// With active channels
		tracker
			.message_queues
			.set_hrmp_channels(create_hrmp_channels(), Default::default());
		let progress = tracker.progress(&mut stats, &metrics, &tracker_storage).await.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::MessageQueues(_, _)))
		);
	}

	#[tokio::test]
	async fn test_includes_current_block_time() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut mock_stats = MockStats::default();
		mock_stats.expect_on_bitfields().returning(|_, _| ());
		mock_stats.expect_on_skipped_slot().returning(|_| ());
		let mut mock_metrics = MockPrometheusMetrics::default();
		mock_metrics.expect_on_bitfields().returning(|_, _, _| ());
		mock_metrics.expect_on_skipped_slot().returning(|_| ());

		// On a block
		tracker.previous_relay_block = Some(Block { num: 41, ts: 1694095326000, hash: H256::random() });
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		tracker.last_non_fork_relay_block_ts = Some(1694095326000);
		mock_stats
			.expect_on_block()
			.with(eq(Duration::from_millis(6000)))
			.once()
			.returning(|_| ());
		mock_metrics
			.expect_on_block()
			.with(eq(Duration::from_secs(6)), eq(100))
			.once()
			.returning(|_, _| ());
		let _progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		// On a fork
		tracker.previous_relay_block = tracker.current_relay_block;
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		let _progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_includes_finality_lag() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut stats = ParachainStats::default();
		let mut mock_metrics = MockPrometheusMetrics::default();
		mock_metrics.expect_on_bitfields().returning(|_, _, _| ());
		mock_metrics.expect_on_skipped_slot().returning(|_| ());
		mock_metrics.expect_on_block().returning(|_, _| ());

		// Without finality lag
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		tracker.finality_lag = None;
		mock_metrics.expect_on_finality_lag().times(0).returning(|_| ());
		let _progress = tracker.progress(&mut stats, &mock_metrics, &tracker_storage).await.unwrap();

		// With finality lag
		tracker.finality_lag = Some(2);
		mock_metrics.expect_on_finality_lag().with(eq(2)).once().returning(|_| ());
		let _progress = tracker.progress(&mut stats, &mock_metrics, &tracker_storage).await.unwrap();
	}

	#[tokio::test]
	async fn test_includes_disputes() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut mock_stats = MockStats::default();
		mock_stats.expect_on_bitfields().returning(|_, _| ());
		mock_stats.expect_on_skipped_slot().returning(|_| ());
		mock_stats.expect_on_block().returning(|_| ());
		let mut mock_metrics = MockPrometheusMetrics::default();
		mock_metrics.expect_on_bitfields().returning(|_, _, _| ());
		mock_metrics.expect_on_skipped_slot().returning(|_| ());
		mock_metrics.expect_on_block().returning(|_, _| ());

		// Without disputes
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		tracker.disputes = vec![];
		mock_stats.expect_on_disputed().times(0).returning(|_| ());
		mock_metrics.expect_on_disputed().times(0).returning(|_, _| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			!progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::Disputed(_)))
		);

		// With disputes
		let dispute = DisputesTracker { candidate: H256::random(), ..Default::default() };
		tracker.disputes = vec![dispute.clone()];
		mock_stats
			.expect_on_disputed()
			.withf(move |d| d.candidate == dispute.candidate)
			.once()
			.returning(|_| ());
		mock_metrics
			.expect_on_disputed()
			.withf(move |d, &id| d.candidate == dispute.candidate && id == 100)
			.once()
			.returning(|_, _| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::Disputed(_)))
		);
	}

	#[tokio::test]
	async fn test_includes_on_demand_order() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut stats = ParachainStats::default();
		let mut mock_metrics = MockPrometheusMetrics::default();
		mock_metrics.expect_on_bitfields().returning(|_, _, _| ());
		mock_metrics.expect_on_skipped_slot().returning(|_| ());
		mock_metrics.expect_on_backed().returning(|_| ());
		mock_metrics.expect_on_block().returning(|_, _| ());

		// With on-demand order
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		let order = OnDemandOrder { para_id: 100, spot_price: 10000 };
		tracker.on_demand_order = Some(order.clone());
		mock_metrics
			.expect_handle_on_demand_order()
			.with(eq(order))
			.once()
			.returning(|_| ());
		let _progress = tracker.progress(&mut stats, &mock_metrics, &tracker_storage).await.unwrap();
		tracker.on_demand_order = None;

		// With on-demand order at block
		tracker.on_demand_order_at = Some(BlockWithoutHash { num: 41, ts: 1694095326000 });
		// If scheduled
		tracker.is_on_demand_scheduled_in_current_block = true;
		mock_metrics
			.expect_handle_on_demand_delay()
			.with(eq(1), eq(100), eq("scheduled"))
			.once()
			.returning(|_, _, _| ());
		mock_metrics
			.expect_handle_on_demand_delay_sec()
			.with(eq(Duration::from_secs(6)), eq(100), eq("scheduled"))
			.once()
			.returning(|_, _, _| ());
		let _progress = tracker.progress(&mut stats, &mock_metrics, &tracker_storage).await.unwrap();
		tracker.is_on_demand_scheduled_in_current_block = false;
		// If backed
		let candidate = create_para_block_info(100, hasher);
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.cores.entry(0).or_default().push(100);
		mock_metrics
			.expect_handle_on_demand_delay()
			.with(eq(1), eq(100), eq("backed"))
			.once()
			.returning(|_, _, _| ());
		mock_metrics
			.expect_handle_on_demand_delay_sec()
			.with(eq(Duration::from_secs(6)), eq(100), eq("backed"))
			.once()
			.returning(|_, _, _| ());
		let _progress = tracker.progress(&mut stats, &mock_metrics, &tracker_storage).await.unwrap();
	}

	#[tokio::test]
	async fn test_includes_candidate_state() {
		let candidate_hash = H256::random();
		let storage = create_storage().await;
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut mock_stats = MockStats::default();
		mock_stats.expect_on_bitfields().returning(|_, _| ());
		mock_stats.expect_on_block().returning(|_| ());
		let mut mock_metrics = MockPrometheusMetrics::default();
		mock_metrics.expect_on_bitfields().returning(|_, _, _| ());
		mock_metrics.expect_on_block().returning(|_, _| ());
		storage_write(
			CollectorPrefixType::Candidate(100),
			candidate_hash,
			create_candidate_record(100, 41, None, H256::random(), 40),
			&storage,
		)
		.await
		.unwrap();

		// When candidate is idle
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		tracker.candidates.entry(0).or_default().push(None);
		mock_stats.expect_on_skipped_slot().once().returning(|_| ());
		mock_metrics.expect_on_skipped_slot().once().returning(|_| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::SkippedSlot))
		);

		// When candidate is backed
		let candidate = ParachainBlockInfo::new(candidate_hash, 0, 0);
		tracker.candidates.clear();
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		mock_stats.expect_on_backed().once().returning(|| ());
		mock_metrics.expect_on_backed().with(eq(100)).once().returning(|_| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(progress.events.iter().any(|e| matches!(e, ParachainConsensusEvent::Backed(_))));

		// When candidate is dropped
		let candidate = tracker.candidates.entry(0).or_default().last_mut().unwrap().as_mut().unwrap();
		candidate.set_dropped();
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(progress.events.iter().any(|e| matches!(e, ParachainConsensusEvent::Dropped(_))));

		// When candidate is pending
		// And data is available
		let candidate = tracker.candidates.entry(0).or_default().last_mut().unwrap().as_mut().unwrap();
		candidate.set_pending();
		candidate.max_availability_bits = 200;
		candidate.current_availability_bits = 140;
		candidate.bitfield_count = 150;
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::PendingAvailability(_)))
		);

		// And availability is slow
		let candidate = tracker.candidates.entry(0).or_default().last_mut().unwrap().as_mut().unwrap();
		candidate.max_availability_bits = 200;
		candidate.current_availability_bits = 120;
		candidate.bitfield_count = 150;
		candidate.core_occupied = true;
		tracker.last_backed_at_block_number = Some(41);
		mock_stats.expect_on_slow_availability().once().returning(|| ());
		mock_metrics
			.expect_on_slow_availability()
			.with(eq(100))
			.once()
			.returning(|_| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::SlowAvailability(_, _)))
		);

		// When candidate is included and data is available
		let candidate = tracker.candidates.entry(0).or_default().last_mut().unwrap().as_mut().unwrap();
		candidate.set_included();
		candidate.max_availability_bits = 200;
		candidate.current_availability_bits = 140;
		candidate.bitfield_count = 150;
		tracker.previous_included_at = Some(BlockWithoutHash { num: 41, ts: 1694095326000 });
		mock_stats
			.expect_on_included()
			.with(eq(42), eq(Some(41)), eq(None))
			.once()
			.returning(|_, _, _| ());
		mock_metrics
			.expect_on_included()
			.with(eq(42), eq(Some(41)), eq(None), eq(Some(Duration::from_secs(6))), eq(100))
			.once()
			.returning(|_, _, _, _, _| ());
		let progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();

		assert!(
			progress
				.events
				.iter()
				.any(|e| matches!(e, ParachainConsensusEvent::Included(_, _, _)))
		);
	}

	#[tokio::test]
	async fn test_inits_disputes_metrics() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);
		let tracker_storage = TrackerStorage::new(100, create_storage().await, hasher);
		let mut mock_stats = MockStats::default();
		mock_stats.expect_on_bitfields().returning(|_, _| ());
		mock_stats.expect_on_block().returning(|_| ());
		mock_stats.expect_on_skipped_slot().returning(|_| ());
		let mut mock_metrics = MockPrometheusMetrics::default();
		mock_metrics.expect_on_bitfields().returning(|_, _, _| ());
		mock_metrics.expect_on_block().returning(|_, _| ());
		mock_metrics.expect_on_skipped_slot().returning(|_| ());

		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		let _progress = tracker
			.progress(&mut mock_stats, &mock_metrics, &tracker_storage)
			.await
			.unwrap();
	}
}

#[cfg(test)]
mod test_logic {
	use crate::test_utils::{create_hasher, create_para_block_info};

	use super::*;

	fn block_with_num(num: u32) -> Block {
		Block { num, hash: Default::default(), ts: Default::default() }
	}

	#[tokio::test]
	async fn test_is_fork() {
		let mut tracker = SubxtTracker::new(100);

		tracker.previous_relay_block = None;
		tracker.current_relay_block = None;
		assert!(!tracker.is_fork());

		tracker.previous_relay_block = None;
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(!tracker.is_fork());

		tracker.previous_relay_block = Some(block_with_num(41));
		tracker.current_relay_block = None;
		assert!(!tracker.is_fork());

		tracker.previous_relay_block = Some(block_with_num(41));
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(!tracker.is_fork());

		tracker.previous_relay_block = Some(block_with_num(42));
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(tracker.is_fork());
	}

	#[tokio::test]
	async fn test_has_backed_candidate() {
		let hasher = create_hasher().await;
		let relay_hash = H256::default();
		let relay_number = 42;
		let mut tracker = SubxtTracker::new(100);
		tracker
			.candidates
			.entry(0)
			.or_default()
			.push(Some(create_para_block_info(100, hasher)));
		tracker.candidates.entry(1).or_default().push(None);

		assert!(tracker.has_backed_candidate(0));
		assert!(!tracker.has_backed_candidate(1));
		assert!(!tracker.has_backed_candidate(2));

		tracker.candidates.clear();
		assert!(!tracker.has_backed_candidate(0));

		tracker.relay_forks.clear();
		tracker.relay_forks.push(ForkTracker {
			relay_hash,
			relay_number,
			backed_candidate: None,
			included_candidate: None,
		});
		assert!(!tracker.has_backed_candidate(0));

		tracker.relay_forks.clear();
		tracker.relay_forks.push(ForkTracker {
			relay_hash,
			relay_number,
			backed_candidate: Some(H256::default()),
			included_candidate: None,
		});
		assert!(tracker.has_backed_candidate(0));

		tracker.relay_forks.clear();
		tracker.relay_forks.push(ForkTracker {
			relay_hash,
			relay_number,
			backed_candidate: None,
			included_candidate: Some(H256::default()),
		});
		assert!(tracker.has_backed_candidate(0));
	}

	#[tokio::test]
	async fn test_is_current_candidate_backed() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);

		assert!(!tracker.is_current_candidate_backed(0));

		tracker.candidates.entry(0).or_default().push(None);
		assert!(!tracker.is_current_candidate_backed(0));

		let candidate = create_para_block_info(100, hasher);
		tracker.candidates.clear();
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		assert!(tracker.is_current_candidate_backed(0));
	}

	#[tokio::test]
	async fn test_is_just_backed() {
		let mut tracker = SubxtTracker::new(100);

		tracker.last_backed_at_block_number = None;
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(!tracker.is_just_backed());

		tracker.last_backed_at_block_number = Some(41);
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(!tracker.is_just_backed());

		tracker.last_backed_at_block_number = Some(42);
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(tracker.is_just_backed());
	}

	#[tokio::test]
	async fn test_is_slow_availability() {
		let hasher = create_hasher().await;
		let mut tracker = SubxtTracker::new(100);

		assert!(!tracker.is_slow_availability(0));

		let mut candidate = create_para_block_info(100, hasher);
		candidate.core_occupied = true;
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.last_backed_at_block_number = Some(42);
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(!tracker.is_slow_availability(0));

		let mut candidate = create_para_block_info(100, hasher);
		candidate.core_occupied = false;
		tracker.candidates.clear();
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.last_backed_at_block_number = Some(41);
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(!tracker.is_slow_availability(0));

		let mut candidate = create_para_block_info(100, hasher);
		candidate.core_occupied = true;
		tracker.candidates.clear();
		tracker.candidates.entry(0).or_default().push(Some(candidate));
		tracker.last_backed_at_block_number = Some(41);
		tracker.current_relay_block = Some(block_with_num(42));
		assert!(tracker.is_slow_availability(0));
	}
}
