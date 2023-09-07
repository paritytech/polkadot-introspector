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
	message_queus_tracker::MessageQueuesTracker,
	parachain_block_info::ParachainBlockInfo,
	prometheus::PrometheusMetrics,
	stats::Stats,
	tracker_rpc::TrackerRpc,
	tracker_storage::TrackerStorage,
	types::{Block, BlockWithoutHash, DisputesTracker, ForkTracker, ParachainConsensusEvent, ParachainProgressUpdate},
	utils::{backed_candidate, extract_availability_bits_count, extract_inherent_fields, time_diff},
};
use log::{error, info};
use polkadot_introspector_essentials::{
	api::subxt_wrapper::SubxtWrapperError,
	collector::{CollectorStorageApi, DisputeInfo},
	metadata::polkadot_primitives,
	types::{BlockNumber, CoreOccupied, OnDemandOrder, Timestamp, H256},
};
use std::{default::Default, time::Duration};
use subxt::error::{Error, MetadataError};

/// A subxt based parachain candidate tracker.
pub struct SubxtTracker {
	/// Parachain ID to track.
	para_id: u32,
	/// API to access collector's storage
	storage: TrackerStorage,

	/// A new session index.
	new_session: Option<u32>,
	/// Information about current parachain block we track.
	current_candidate: ParachainBlockInfo,
	/// Current relay chain block.
	current_relay_block: Option<Block>,
	/// Previous relay chain block.
	previous_relay_block: Option<Block>,

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
	pub fn new(para_id: u32, api: CollectorStorageApi) -> Self {
		Self {
			para_id,
			storage: TrackerStorage::new(para_id, api),
			current_candidate: Default::default(),
			new_session: None,
			current_relay_block: None,
			previous_relay_block: None,
			on_demand_order: None,
			on_demand_order_at: None,
			is_on_demand_scheduled_in_current_block: false,
			finality_lag: None,
			disputes: Vec::new(),
			last_backed_at_block_number: None,
			last_non_fork_relay_block_ts: None,
			last_included_at: None,
			previous_included_at: None,
			message_queues: Default::default(),
			relay_forks: vec![],
		}
	}

	/// Proccesses relay chain block:
	/// 	- injects block information to tracker's state,
	/// 	- notifies about changes using progress, stats and metrics,
	/// 	- resets state.
	pub async fn process_new_head(
		&mut self,
		block_hash: H256,
		block_number: BlockNumber,
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
		rpc: &mut impl TrackerRpc,
	) -> color_eyre::Result<Option<ParachainProgressUpdate>> {
		self.inject_block(block_hash, block_number, rpc).await?;
		let progress = self.progress(stats, metrics).await;
		self.maybe_reset_state();

		Ok(progress)
	}

	/// Saves new session to tracker's state
	pub fn process_new_session(&mut self, session_index: u32) {
		self.new_session = Some(session_index)
	}

	/// Injects a new relay chain block into the tracker. Blocks must be injected in order.
	async fn inject_block(
		&mut self,
		block_hash: H256,
		block_number: BlockNumber,
		rpc: &mut impl TrackerRpc,
	) -> color_eyre::Result<()> {
		if let Some(inherent) = self.storage.inherent_data(block_hash).await {
			let (bitfields, backed_candidates, disputes) = extract_inherent_fields(inherent);

			self.set_relay_block(block_hash, block_number, rpc).await?;
			self.set_forks(block_hash, block_number);

			self.set_current_candidate(backed_candidates, bitfields.len(), block_number);
			self.set_core_assignment(block_hash, rpc).await?;
			self.set_disputes(&disputes[..]).await;

			self.set_hrmp_channels(block_hash, rpc).await?;
			self.set_on_demand_order(block_hash).await;

			// If a candidate was backed in this relay block, we don't need to process availability now.
			if self.has_backed_candidate() && !self.is_just_backed() {
				self.set_availability(block_hash, bitfields, rpc).await?;
			}
		} else {
			error!("Failed to get inherent data for {:?}", block_hash);
		}

		Ok(())
	}

	/// Creates a parachain progress.
	async fn progress(
		&self,
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
	) -> Option<ParachainProgressUpdate> {
		if let Some(block) = self.current_relay_block {
			let prev_timestamp = self.last_non_fork_relay_block_ts.unwrap_or(block.ts);
			let mut progress = ParachainProgressUpdate {
				timestamp: block.ts,
				prev_timestamp,
				block_number: block.num,
				block_hash: block.hash,
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
			self.notify_current_block_time(stats, metrics);
			self.notify_finality_lag(metrics);
			self.notify_on_demand_order(metrics);

			Some(progress)
		} else {
			None
		}
	}

	/// Resets state
	fn maybe_reset_state(&mut self) {
		if self.current_candidate.is_backed() {
			self.on_demand_order_at = None;
		}
		self.new_session = None;
		self.on_demand_order = None;
		self.is_on_demand_scheduled_in_current_block = false;
		self.disputes.clear();
		self.current_candidate.maybe_reset();
	}

	async fn set_hrmp_channels(&mut self, block_hash: H256, rpc: &mut impl TrackerRpc) -> color_eyre::Result<()> {
		let inbound = rpc.inbound_hrmp_channels(block_hash).await?;
		let outbound = rpc.outbound_hrmp_channels(block_hash).await?;
		self.message_queues.set_hrmp_channels(inbound, outbound);

		Ok(())
	}

	async fn set_relay_block(
		&mut self,
		block_hash: H256,
		block_number: BlockNumber,
		rpc: &mut impl TrackerRpc,
	) -> color_eyre::Result<()> {
		let ts = rpc.block_timestamp(block_hash).await?;
		self.previous_relay_block = self.current_relay_block;
		self.current_relay_block = Some(Block { num: block_number, ts, hash: block_hash });

		if !self.is_fork() {
			self.last_non_fork_relay_block_ts = Some(ts);
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

	fn set_current_candidate(
		&mut self,
		backed_candidates: Vec<polkadot_primitives::BackedCandidate<H256>>,
		bitfields_count: usize,
		block_number: BlockNumber,
	) {
		self.current_candidate.bitfield_count = bitfields_count as u32;
		if let Some(candidate) = backed_candidate(backed_candidates, self.para_id) {
			self.current_candidate.set_backed();
			self.current_candidate.set_candidate(candidate);
			self.last_backed_at_block_number = Some(block_number);

			if let Some(current_fork) = self.relay_forks.last_mut() {
				current_fork.backed_candidate = self.current_candidate.candidate_hash;
			}
		} else if !self.has_backed_candidate() {
			self.current_candidate.set_idle();
		}
	}

	async fn set_core_assignment(&mut self, block_hash: H256, rpc: &mut impl TrackerRpc) -> color_eyre::Result<()> {
		// After adding On-demand Parachains, `ParaScheduler.Scheduled` API call will be removed
		let assignments = match rpc.core_assignments_via_scheduled_paras(block_hash).await {
			// `ParaScheduler,Scheduled` not found, try to fetch `ParaScheduler.ClaimQueue`
			Err(SubxtWrapperError::SubxtError(Error::Metadata(MetadataError::StorageEntryNotFound(_)))) =>
				rpc.core_assignments_via_claim_queue(block_hash).await,
			v => v,
		}?;
		if let Some((&core, scheduled_ids)) = assignments.iter().find(|(_, ids)| ids.contains(&self.para_id)) {
			self.current_candidate.assigned_core = Some(core);
			self.current_candidate.core_occupied =
				matches!(rpc.occupied_cores(block_hash).await?[core as usize], CoreOccupied::Paras);
			self.is_on_demand_scheduled_in_current_block =
				self.on_demand_order.is_some() && scheduled_ids[0] == self.para_id;
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
					self.disputes.push(DisputesTracker::new(
						dispute_info,
						outcome,
						initiated,
						initiator_indices,
						concluded,
						self.storage.session_keys(dispute_info.session).await.as_ref(),
						self.storage.session_keys(session_index).await.as_ref(),
					));
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
		rpc: &mut impl TrackerRpc,
	) -> color_eyre::Result<()> {
		if self.current_candidate.is_backed() {
			// We only process availability if our parachain is assigned to an availability core.
			if let Some(core) = self.current_candidate.assigned_core {
				self.current_candidate.current_availability_bits = extract_availability_bits_count(bitfields, core);
				self.current_candidate.max_availability_bits =
					self.validators_indices(block_hash, rpc).await?.len() as u32;

				if self.current_candidate.is_data_available() {
					self.current_candidate.set_included();
					self.relay_forks.last_mut().expect("must have relay fork").included_candidate =
						self.current_candidate.candidate_hash;
					self.previous_included_at = self.last_included_at;
					self.last_included_at = self.current_relay_block.map(|v| v.into());
				} else {
					self.current_candidate.set_pending();
				}
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
			metrics.on_block(ts.as_secs_f64(), self.para_id);
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
			if self.current_candidate.is_backed() {
				metrics.handle_on_demand_delay(delay, self.para_id, "backed");
			}
		}
		if let Some(delay_sec) = self.on_demand_delay_sec() {
			if self.is_on_demand_scheduled_in_current_block {
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
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
	) {
		if self.current_candidate.is_bitfield_propagation_slow() {
			progress.events.push(ParachainConsensusEvent::SlowBitfieldPropagation(
				self.current_candidate.bitfield_count,
				self.current_candidate.max_availability_bits,
			))
		}
		stats
			.on_bitfields(self.current_candidate.bitfield_count, self.current_candidate.is_bitfield_propagation_slow());
		metrics.on_bitfields(
			self.current_candidate.bitfield_count,
			self.current_candidate.is_bitfield_propagation_slow(),
			self.para_id,
		);
	}

	async fn notify_candidate_state(
		&self,
		progress: &mut ParachainProgressUpdate,
		stats: &mut impl Stats,
		metrics: &impl PrometheusMetrics,
	) {
		if self.current_candidate.is_idle() {
			progress.events.push(ParachainConsensusEvent::SkippedSlot);
			stats.on_skipped_slot(progress);
			metrics.on_skipped_slot(progress);
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
					let relay_block = self.current_relay_block.expect("Checked by caller; qed");
					stats.on_included(relay_block.num, self.previous_included_at.map(|v| v.num), backed_in);
					metrics.on_included(
						relay_block.num,
						self.previous_included_at.map(|v| v.num),
						backed_in,
						time_diff(Some(relay_block.ts), self.previous_included_at.map(|v| v.ts)),
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

	fn current_block_time(&self) -> Duration {
		let cur_ts = self.current_relay_block.map(|v| v.ts).unwrap_or_default();
		let base_ts = self.last_non_fork_relay_block_ts.unwrap_or(cur_ts);
		Duration::from_millis(cur_ts).saturating_sub(Duration::from_millis(base_ts))
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

	fn has_backed_candidate(&self) -> bool {
		self.current_candidate.candidate.is_some() ||
			self.relay_forks
				.iter()
				.any(|fork| fork.backed_candidate.is_some() || fork.included_candidate.is_some())
	}

	fn is_just_backed(&self) -> bool {
		self.last_backed_at_block_number.is_some() &&
			self.last_backed_at_block_number == self.current_relay_block.map(|v| v.num)
	}

	fn is_slow_availability(&self) -> bool {
		self.current_candidate.core_occupied &&
			self.last_backed_at_block_number != self.current_relay_block.map(|v| v.num)
	}

	async fn validators_indices(
		&mut self,
		block_hash: H256,
		rpc: &mut impl TrackerRpc,
	) -> color_eyre::Result<Vec<polkadot_primitives::ValidatorIndex>> {
		Ok(rpc.backing_groups(block_hash).await?.into_iter().flatten().collect())
	}

	async fn candidate_backed_in(&self, candidate_hash: H256) -> Option<u32> {
		self.storage.candidate(candidate_hash).await.map(|v| {
			v.candidate_inclusion
				.backed
				.saturating_sub(v.candidate_inclusion.relay_parent_number)
		})
	}
}

#[cfg(test)]
mod test_process_new_session {
	use super::*;
	use crate::test_utils::create_storage_api;

	#[tokio::test]
	async fn test_sets_new_session() {
		let mut tracker = SubxtTracker::new(100, create_storage_api());
		assert!(tracker.new_session.is_none());

		tracker.process_new_session(42);

		assert_eq!(tracker.new_session, Some(42));
	}
}

#[cfg(test)]
mod test_maybe_reset_state {
	use super::*;
	use crate::test_utils::create_storage_api;

	#[tokio::test]
	async fn test_resets_state_if_not_backed() {
		let mut tracker = SubxtTracker::new(100, create_storage_api());
		tracker.current_candidate.set_idle();
		tracker.new_session = Some(42);
		tracker.on_demand_order = Some(OnDemandOrder::default());
		tracker.is_on_demand_scheduled_in_current_block = true;
		tracker.disputes = vec![DisputesTracker::default()];
		assert!(!tracker.current_candidate.is_reset);

		tracker.maybe_reset_state();

		assert!(tracker.new_session.is_none());
		assert!(tracker.on_demand_order.is_none());
		assert!(!tracker.is_on_demand_scheduled_in_current_block);
		assert!(tracker.disputes.is_empty());
		assert!(tracker.current_candidate.is_reset);
	}

	#[tokio::test]
	async fn test_resets_state_if_backed() {
		let mut tracker = SubxtTracker::new(100, create_storage_api());
		tracker.current_candidate.set_backed();
		tracker.new_session = Some(42);
		tracker.on_demand_order = Some(OnDemandOrder::default());
		tracker.on_demand_order_at = Some(BlockWithoutHash::default());
		tracker.is_on_demand_scheduled_in_current_block = true;
		tracker.disputes = vec![DisputesTracker::default()];
		assert!(!tracker.current_candidate.is_reset);

		tracker.maybe_reset_state();

		assert!(tracker.on_demand_order_at.is_none());
		assert!(tracker.new_session.is_none());
		assert!(tracker.on_demand_order.is_none());
		assert!(!tracker.is_on_demand_scheduled_in_current_block);
		assert!(tracker.disputes.is_empty());
		assert!(tracker.current_candidate.is_reset);
	}
}

#[cfg(test)]
mod test_inject_block {
	use super::*;
	use crate::{
		test_utils::{create_inherent_data, create_storage_api, storage_write},
		tracker_rpc::MockTrackerRpc,
	};
	use polkadot_introspector_essentials::collector::CollectorPrefixType;

	#[tokio::test]
	async fn test_changes_nothing_if_there_is_no_inherent_data() {
		let hash = H256::random();
		let mut tracker = SubxtTracker::new(100, create_storage_api());
		let mut mock_rpc = MockTrackerRpc::new();

		tracker.inject_block(hash, 0, &mut mock_rpc).await.unwrap();

		assert!(tracker.new_session.is_none());
		assert!(tracker.current_candidate.candidate.is_none());
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
		let api = create_storage_api();
		let mut tracker = SubxtTracker::new(100, api.clone());
		let mut mock_rpc = MockTrackerRpc::new();
		mock_rpc
			.expect_core_assignments_via_scheduled_paras()
			.returning(|_| Ok(Default::default()));
		mock_rpc.expect_inbound_hrmp_channels().returning(|_| Ok(Default::default()));
		mock_rpc.expect_outbound_hrmp_channels().returning(|_| Ok(Default::default()));

		// Inject a block
		storage_write(CollectorPrefixType::InherentData, first_hash, create_inherent_data(100), &api)
			.await
			.unwrap();
		mock_rpc.expect_block_timestamp().returning(|_| Ok(1694095332000));
		tracker.inject_block(first_hash, 42, &mut mock_rpc).await.unwrap();

		let current = tracker.current_relay_block.unwrap();
		assert!(tracker.previous_relay_block.is_none());
		assert_eq!(current.hash, first_hash);
		assert_eq!(tracker.last_non_fork_relay_block_ts, Some(1694095332000));
		assert!(tracker.finality_lag.is_none());

		// Inject a fork and relevant finalized block number
		storage_write(CollectorPrefixType::InherentData, second_hash, create_inherent_data(100), &api)
			.await
			.unwrap();
		storage_write(CollectorPrefixType::RelevantFinalizedBlockNumber, second_hash, 40, &api)
			.await
			.unwrap();
		mock_rpc.expect_block_timestamp().returning(|_| Ok(1694095333000));
		tracker.inject_block(second_hash, 42, &mut mock_rpc).await.unwrap();

		let previous = tracker.previous_relay_block.unwrap();
		let current = tracker.current_relay_block.unwrap();
		assert_eq!(previous.hash, first_hash);
		assert_eq!(current.hash, second_hash);
		assert_eq!(tracker.last_non_fork_relay_block_ts, Some(1694095332000));
		assert_eq!(tracker.finality_lag, Some(2));
	}
}

#[cfg(test)]
mod test_progress {
	use super::*;
	use crate::{
		prometheus::{Metrics, MockPrometheusMetrics},
		stats::{MockStats, ParachainStats},
		test_utils::{create_inherent_data, create_storage_api, storage_write},
		tracker_rpc::MockTrackerRpc,
	};
	use mockall::predicate::eq;
	use polkadot_introspector_essentials::collector::CollectorPrefixType;

	#[tokio::test]
	async fn test_returns_none_if_no_current_block() {
		let tracker = SubxtTracker::new(100, create_storage_api());
		let mut stats = MockStats::default();
		let metrics = Metrics::default();

		let progress = tracker.progress(&mut stats, &metrics).await;

		assert!(progress.is_none());
	}

	#[tokio::test]
	async fn test_returns_progress_on_current_block() {
		let hash = H256::random();
		let mut tracker = SubxtTracker::new(100, create_storage_api());
		let mut stats = ParachainStats::default();
		let metrics = Metrics::default();

		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash });
		let progress = tracker.progress(&mut stats, &metrics).await.unwrap();

		assert_eq!(progress.timestamp, 1694095332000);
		assert_eq!(progress.prev_timestamp, 1694095332000);
		assert_eq!(progress.block_number, 42);
		assert_eq!(progress.block_hash, hash);
		assert_eq!(progress.para_id, 100);
		assert!(!progress.is_fork);
		assert!(progress.finality_lag.is_none());
		assert!(!progress.core_occupied);
	}

	#[tokio::test]
	async fn test_includes_new_session_if_exist() {
		let mut tracker = SubxtTracker::new(100, create_storage_api());
		let mut stats = ParachainStats::default();
		let metrics = Metrics::default();

		// No new session
		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		let progress = tracker.progress(&mut stats, &metrics).await.unwrap();

		assert!(!progress
			.events
			.iter()
			.any(|e| matches!(e, ParachainConsensusEvent::NewSession(_))));

		// With new session
		tracker.process_new_session(12);
		let progress = tracker.progress(&mut stats, &metrics).await.unwrap();

		assert!(progress
			.events
			.iter()
			.any(|e| matches!(e, ParachainConsensusEvent::NewSession(_))));
	}

	#[tokio::test]
	async fn test_includes_core_assignment() {
		let mut tracker = SubxtTracker::new(100, create_storage_api());
		let mut stats = ParachainStats::default();
		let metrics = Metrics::default();

		tracker.current_relay_block = Some(Block { num: 42, ts: 1694095332000, hash: H256::random() });
		tracker.current_candidate.assigned_core = Some(0);
		tracker.current_candidate.core_occupied = true;
		let progress = tracker.progress(&mut stats, &metrics).await.unwrap();

		assert!(progress
			.events
			.iter()
			.any(|e| matches!(e, ParachainConsensusEvent::CoreAssigned(0))));
	}

	#[tokio::test]
	async fn test_includes_slow_propogation() {
		let mut tracker = SubxtTracker::new(100, create_storage_api());
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
		tracker.current_candidate.bitfield_count = 120;
		mock_stats.expect_on_bitfields().with(eq(120), eq(false)).returning(|_, _| ());
		mock_metrics
			.expect_on_bitfields()
			.with(eq(120), eq(false), eq(100))
			.returning(|_, _, _| ());
		let progress = tracker.progress(&mut mock_stats, &mock_metrics).await.unwrap();

		assert!(!progress
			.events
			.iter()
			.any(|e| matches!(e, ParachainConsensusEvent::SlowBitfieldPropagation(_, _))));

		// Bitfields propogation is slow
		tracker.current_candidate.set_backed();
		tracker.current_candidate.max_availability_bits = 200;
		mock_stats.expect_on_bitfields().with(eq(120), eq(true)).returning(|_, _| ());
		mock_metrics
			.expect_on_bitfields()
			.with(eq(120), eq(true), eq(100))
			.returning(|_, _, _| ());
		let progress = tracker.progress(&mut mock_stats, &mock_metrics).await.unwrap();

		assert!(progress
			.events
			.iter()
			.any(|e| matches!(e, ParachainConsensusEvent::SlowBitfieldPropagation(_, _))));
	}

	// 	TODO: notify_candidate_state
	// 	TODO: notify_disputes
	// 	TODO: notify_active_message_queues
	// 	TODO: notify_current_block_time
	// 	TODO: notify_finality_lag
	// 	TODO: notify_on_demand_order
}
