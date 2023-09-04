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
	parachain_block_info::ParachainBlockInfo,
	utils::{
		backed_candidate, extract_availability_bits_count, extract_inherent_fields, extract_misbehaving_validators,
		extract_validator_addresses, extract_votes,
	},
};

use super::{
	progress::{ParachainConsensusEvent, ParachainProgressUpdate},
	prometheus::Metrics,
	stats::ParachainStats,
	utils::time_diff,
};
use log::{debug, error, info};
use parity_scale_codec::{Decode, Encode};
use polkadot_introspector_essentials::{
	api::subxt_wrapper::{InherentData, RequestExecutor, SubxtHrmpChannel, SubxtWrapperError},
	chain_events::SubxtDisputeResult,
	collector::{candidate_record::CandidateRecord, CollectorPrefixType, CollectorStorageApi, DisputeInfo},
	metadata::polkadot_primitives::{self, ValidatorIndex},
	types::{AccountId32, BlockNumber, CoreOccupied, OnDemandOrder, Timestamp, H256},
};
use std::{
	collections::{BTreeMap, HashMap},
	default::Default,
	fmt::Debug,
	time::Duration,
};
use subxt::{
	config::{substrate::BlakeTwo256, Hasher},
	error::{Error, MetadataError},
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
	) -> color_eyre::Result<()>;

	/// Called when a new session is observed
	async fn inject_session(&mut self, session_index: u32);

	/// Update current parachain progress.
	async fn progress(&mut self, metrics: &Metrics) -> Option<ParachainProgressUpdate>;
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
	/// A vector of validators initiateds the dispute (index + identify)
	pub initiators: Vec<(u32, String)>,
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
	fn update_hrmp_channels(
		&mut self,
		inbound_channels: BTreeMap<u32, SubxtHrmpChannel>,
		outbound_channels: BTreeMap<u32, SubxtHrmpChannel>,
	) {
		debug!("hrmp channels configured: {:?} in, {:?} out", &inbound_channels, &outbound_channels);
		self.inbound_hrmp_channels = inbound_channels;
		self.outbound_hrmp_channels = outbound_channels;
	}

	/// Returns if there are HRMP messages in any direction
	fn has_hrmp_messages(&self) -> bool {
		self.inbound_hrmp_channels.values().any(|channel| channel.total_size > 0) ||
			self.outbound_hrmp_channels.values().any(|channel| channel.total_size > 0)
	}

	/// Returns active inbound channels
	fn active_inbound_channels(&self) -> Vec<(u32, SubxtHrmpChannel)> {
		self.inbound_hrmp_channels
			.iter()
			.filter(|(_, queue)| queue.total_size > 0)
			.map(|(source_id, queue)| (*source_id, queue.clone()))
			.collect::<Vec<_>>()
	}

	/// Returns active outbound channels
	fn active_outbound_channels(&self) -> Vec<(u32, SubxtHrmpChannel)> {
		self.outbound_hrmp_channels
			.iter()
			.filter(|(_, queue)| queue.total_size > 0)
			.map(|(dest_id, queue)| (*dest_id, queue.clone()))
			.collect::<Vec<_>>()
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
	/// The relay chain block number at which the last candidate was backed at.
	last_backed_at_block_number: Option<BlockNumber>,
	/// The relay chain timestamp at which the previous candidate was included at.
	previous_included_at_ts: Option<Timestamp>,
	/// The relay chain timestamp at which the last candidate was included at.
	last_included_at_ts: Option<Timestamp>,
	/// Information about current block we track.
	current_candidate: ParachainBlockInfo,
	/// Current relay chain block.
	current_relay_block: Option<(BlockNumber, H256)>,
	/// Previous relay chain block
	previous_relay_block: Option<(BlockNumber, H256)>,
	/// Disputes information if any disputes are there.
	disputes: Vec<DisputesTracker>,
	/// Current relay chain block timestamp.
	current_relay_block_ts: Option<Timestamp>,
	/// Current on-demand order
	on_demand_order: Option<OnDemandOrder>,
	/// Relay block where the last on-demand order was placed
	on_demand_order_block_number: Option<BlockNumber>,
	/// Timestamp where the last on-demand order was placed
	on_demand_order_ts: Option<Timestamp>,
	/// On-demand parachain was scheduled in current relay block
	on_demand_scheduled: bool,
	/// Last observed finality lag
	finality_lag: Option<u32>,
	/// Last relay chain block timestamp.
	last_relay_block_ts: Option<Timestamp>,
	/// Previous included candidate in relay parent number
	previous_included_at_block_number: Option<BlockNumber>,
	/// Last included candidate in relay parent number
	last_included_at_block_number: Option<BlockNumber>,
	/// Messages queues status
	message_queues: SubxtMessageQueuesTracker,

	/// Parachain statistics. Used to print summary at the end of a run.
	stats: ParachainStats,
	/// Parachain progress update.
	progress: Option<ParachainProgressUpdate>,
	/// Current forks recorded
	relay_forks: Vec<ForkTracker>,
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
	) -> color_eyre::Result<()> {
		if let Some(inherent) = self.read_inherent_data(block_hash).await {
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

	async fn inject_session(&mut self, session_index: u32) {
		if let Some(progress) = self.progress.as_mut() {
			progress.events.push(ParachainConsensusEvent::NewSession(session_index));
		}
	}

	async fn progress(&mut self, metrics: &Metrics) -> Option<ParachainProgressUpdate> {
		if self.current_relay_block.is_some() {
			self.init_progress();

			// self.set_relay_block(block_hash, block_number).await?;
			// self.set_forks(block_hash, block_number);

			// self.set_current_candidate(backed_candidates, bitfields.len(), block_number);
			// self.set_core_assignment(block_hash).await?;
			// self.set_disputes(&disputes[..]).await;

			// self.set_hrmp_channels(block_hash).await?;
			// self.set_on_demand_order(block_hash).await;

			// self.set_availability(block_hash, bitfields).await?;

			self.process_core_assignment();
			self.process_core_occupied();
			self.process_bitfield_propagation(metrics);
			self.process_candidate_state(metrics).await;
			self.process_disputes(metrics);
			self.process_active_message_queues();
			self.process_block_ts(metrics);
			self.process_finality_lag(metrics);
			self.process_on_demand_order(metrics);
		} else {
			self.skip_progress();
		}

		self.progress.clone()
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
			on_demand_order: None,
			on_demand_order_block_number: None,
			on_demand_order_ts: None,
			on_demand_scheduled: false,
			finality_lag: None,
			disputes: Vec::new(),
			last_backed_at_block_number: None,
			last_relay_block_ts: None,
			last_included_at_block_number: None,
			last_included_at_ts: None,
			previous_included_at_block_number: None,
			previous_included_at_ts: None,
			message_queues: Default::default(),
			progress: None,
			relay_forks: vec![],
		}
	}

	/// Called to move to idle state after inclusion/timeout.
	pub fn maybe_reset_state(&mut self) {
		if self.current_candidate.is_backed() {
			self.on_demand_order_block_number = None;
			self.on_demand_order_ts = None;
		}
		self.on_demand_order = None;
		self.on_demand_scheduled = false;
		self.disputes.clear();
		self.progress = None;
		self.current_candidate.maybe_reset();
	}

	/// Returns the stats
	pub fn summary(&self) -> &ParachainStats {
		&self.stats
	}

	fn skip_progress(&mut self) {
		self.progress = None
	}

	fn init_progress(&mut self) {
		if let Some((block_number, block_hash)) = self.current_relay_block {
			self.progress = Some(ParachainProgressUpdate::new(
				self.para_id,
				self.current_relay_block_ts,
				self.last_relay_block_ts,
				block_number,
				block_hash,
				self.is_fork(),
				self.finality_lag,
			));
		}
	}

	fn process_disputes(&mut self, metrics: &Metrics) {
		self.disputes.iter().for_each(|outcome| {
			if let Some(progress) = self.progress.as_mut() {
				progress.events.push(ParachainConsensusEvent::Disputed(outcome.clone()));
			}
			self.stats.on_disputed(outcome);
			metrics.on_disputed(outcome, self.para_id);
		});
	}

	fn process_active_message_queues(&mut self) {
		if self.message_queues.has_hrmp_messages() {
			if let Some(progress) = self.progress.as_mut() {
				progress.events.push(ParachainConsensusEvent::MessageQueues(
					self.message_queues.active_inbound_channels(),
					self.message_queues.active_outbound_channels(),
				))
			}
		}
	}

	fn process_block_ts(&mut self, metrics: &Metrics) {
		if !self.is_fork() {
			let ts = self.current_ts();
			self.stats.on_block(ts);
			metrics.on_block(ts.as_secs_f64(), self.para_id);
		}
	}

	fn process_finality_lag(&mut self, metrics: &Metrics) {
		if let Some(finality_lag) = self.finality_lag {
			metrics.on_finality_lag(finality_lag);
		}
	}

	fn process_on_demand_order(&mut self, metrics: &Metrics) {
		if let Some(ref order) = self.on_demand_order {
			metrics.handle_on_demand_order(order);
		}
		if let Some(delay) = self.on_demand_delay() {
			if self.on_demand_scheduled {
				metrics.handle_on_demand_delay(delay, self.para_id, "scheduled");
			}
			if self.current_candidate.is_backed() {
				metrics.handle_on_demand_delay(delay, self.para_id, "backed");
			}
		}
		if let Some(delay_sec) = self.on_demand_delay_ts() {
			if self.on_demand_scheduled {
				metrics.handle_on_demand_delay_sec(delay_sec, self.para_id, "scheduled");
			}
			if self.current_candidate.is_backed() {
				metrics.handle_on_demand_delay_sec(delay_sec, self.para_id, "backed");
			}
		}
	}

	async fn set_hrmp_channels(&mut self, block_hash: H256) -> color_eyre::Result<()> {
		let inbound = self.fetch_inbound_hrmp_channels(block_hash).await?;
		let outbound = self.fetch_outbound_hrmp_channels(block_hash).await?;
		self.message_queues.update_hrmp_channels(inbound, outbound);

		Ok(())
	}

	async fn set_relay_block(&mut self, block_hash: H256, block_number: BlockNumber) -> color_eyre::Result<()> {
		self.previous_relay_block = self.current_relay_block;
		self.current_relay_block = Some((block_number, block_hash));

		self.current_relay_block_ts = Some(self.fetch_block_timestamp(block_hash).await?);
		if !self.is_fork() {
			self.last_relay_block_ts = self.current_relay_block_ts;
		}

		self.finality_lag = self
			.read_relevant_finalized_block_number(block_hash)
			.await
			.map(|num| block_number - num);

		Ok(())
	}

	async fn set_on_demand_order(&mut self, block_hash: H256) {
		self.on_demand_order = self.read_on_demand_order(block_hash).await;
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
		let assignments = match self.fetch_core_assignments_via_scheduled_paras(block_hash).await {
			Ok(v) => v,
			// The `ParaScheduler,Scheduled` API call not found,
			// we should try to fetch `ParaScheduler,ClaimQueue` instead
			Err(SubxtWrapperError::SubxtError(Error::Metadata(MetadataError::StorageEntryNotFound(_)))) =>
				self.fetch_core_assignments_via_claim_queue(block_hash).await?,
			Err(e) => return Err(e.into()),
		};
		if let Some((&core, scheduled_ids)) = assignments.iter().find(|(_, ids)| ids.contains(&self.para_id)) {
			self.current_candidate.assigned_core = Some(core);
			self.current_candidate.core_occupied =
				matches!(self.fetch_occupied_cores(block_hash).await?[core as usize], CoreOccupied::Paras);
			self.on_demand_scheduled = self.on_demand_order.is_some() && scheduled_ids[0] == self.para_id;
		}
		Ok(())
	}

	async fn set_disputes(&mut self, disputes: &[polkadot_primitives::DisputeStatementSet]) {
		self.disputes = Vec::with_capacity(disputes.len());
		for dispute_info in disputes {
			if let Some(DisputeInfo { outcome, session_index, initiated, initiator_indices, concluded, .. }) =
				self.read_dispute(dispute_info.candidate_hash.0).await
			{
				if let Some(outcome) = outcome {
					let session_info = self.read_session_keys(dispute_info.session).await;
					// TODO: we would like to distinguish different dispute phases at some point
					let (voted_for, voted_against) = extract_votes(&dispute_info);
					let misbehaving_validators = extract_misbehaving_validators(
						session_info.as_ref(),
						dispute_info,
						outcome == SubxtDisputeResult::Valid,
					);
					let initiators_session_info = self.read_session_keys(session_index).await;
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

	fn process_core_assignment(&mut self) {
		if let Some(assigned_core) = self.current_candidate.assigned_core {
			if let Some(progress) = self.progress.as_mut() {
				progress.events.push(ParachainConsensusEvent::CoreAssigned(assigned_core));
			}
		}
	}

	fn process_core_occupied(&mut self) {
		if let Some(progress) = self.progress.as_mut() {
			progress.core_occupied = self.current_candidate.core_occupied;
		}
	}

	fn process_bitfield_propagation(&mut self, metrics: &Metrics) {
		// This makes sense to show if we have a relay chain block and pipeline not idle.
		if self.current_relay_block.is_some() {
			// If `max_availability_bits` is not set do not check for bitfield propagation.
			// Usually this happens at startup, when we miss a core assignment and we do not update
			// availability before calling this function.
			if self.current_candidate.is_bitfield_propagation_low() {
				if let Some(progress) = self.progress.as_mut() {
					progress.events.push(ParachainConsensusEvent::SlowBitfieldPropagation(
						self.current_candidate.bitfield_count,
						self.current_candidate.max_availability_bits,
					))
				}
			}
			self.stats.on_bitfields(
				self.current_candidate.bitfield_count,
				self.current_candidate.is_bitfield_propagation_low(),
			);
			metrics.on_bitfields(
				self.current_candidate.bitfield_count,
				self.current_candidate.is_bitfield_propagation_low(),
				self.para_id,
			);
		}
	}

	async fn process_candidate_state(&mut self, metrics: &Metrics) {
		if self.current_candidate.is_idle() {
			if let Some(progress) = self.progress.as_mut() {
				progress.events.push(ParachainConsensusEvent::SkippedSlot);
				self.stats.on_skipped_slot(progress);
				metrics.on_skipped_slot(progress);
			}
		}

		if self.current_candidate.is_backed() {
			if let Some(candidate_hash) = self.current_candidate.candidate_hash {
				if let Some(progress) = self.progress.as_mut() {
					progress.events.push(ParachainConsensusEvent::Backed(candidate_hash));
				}
				self.stats.on_backed();
				metrics.on_backed(self.para_id);
			}
		}

		if self.current_candidate.is_pending() || self.current_candidate.is_included() {
			if let Some(progress) = self.progress.as_mut() {
				progress.bitfield_health.max_bitfield_count = self.current_candidate.max_availability_bits;
				progress.bitfield_health.available_count = self.current_candidate.current_availability_bits;
				progress.bitfield_health.bitfield_count = self.current_candidate.bitfield_count;
			}

			if self.current_candidate.is_data_available() {
				if let Some(candidate_hash) = self.current_candidate.candidate_hash {
					if let Some(progress) = self.progress.as_mut() {
						progress.events.push(ParachainConsensusEvent::Included(
							candidate_hash,
							self.current_candidate.current_availability_bits,
							self.current_candidate.max_availability_bits,
						));
					}

					let backed_in = self.candidate_backed_in(candidate_hash).await;
					let (relay_block_number, _) = self.current_relay_block.expect("Checked by caller; qed");
					self.stats
						.on_included(relay_block_number, self.previous_included_at_block_number, backed_in);
					metrics.on_included(
						relay_block_number,
						self.previous_included_at_block_number,
						backed_in,
						time_diff(self.current_relay_block_ts, self.previous_included_at_ts),
						self.para_id,
					);
				}
			} else if self.is_slow_availability() {
				if let Some(progress) = self.progress.as_mut() {
					progress.events.push(ParachainConsensusEvent::SlowAvailability(
						self.current_candidate.current_availability_bits,
						self.current_candidate.max_availability_bits,
					));
				}
				self.stats.on_slow_availability();
				metrics.on_slow_availability(self.para_id);
			}
		}
	}

	async fn fetch_inbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		self.executor
			.get_inbound_hrmp_channels(self.node_rpc_url.as_str(), block_hash, self.para_id)
			.await
	}

	async fn fetch_outbound_hrmp_channels(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<BTreeMap<u32, SubxtHrmpChannel>, SubxtWrapperError> {
		self.executor
			.get_outbound_hrmp_channels(self.node_rpc_url.as_str(), block_hash, self.para_id)
			.await
	}

	async fn fetch_core_assignments_via_scheduled_paras(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<HashMap<u32, Vec<u32>>, SubxtWrapperError> {
		let core_assignments = self
			.executor
			.get_scheduled_paras(self.node_rpc_url.as_str(), block_hash)
			.await?;

		Ok(core_assignments
			.iter()
			.map(|v| (v.core.0, vec![v.para_id.0]))
			.collect::<HashMap<_, _>>())
	}

	async fn fetch_core_assignments_via_claim_queue(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<HashMap<u32, Vec<u32>>, SubxtWrapperError> {
		let assignments = self.executor.get_claim_queue(self.node_rpc_url.as_str(), block_hash).await?;
		Ok(assignments
			.iter()
			.map(|(core, queue)| {
				let ids = queue
					.iter()
					.filter_map(|v| v.as_ref().map(|v| v.assignment.para_id))
					.collect::<Vec<_>>();
				(*core, ids)
			})
			.collect())
	}

	async fn fetch_backing_groups(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Vec<Vec<ValidatorIndex>>, SubxtWrapperError> {
		self.executor.get_backing_groups(self.node_rpc_url.as_str(), block_hash).await
	}

	async fn fetch_block_timestamp(&mut self, block_hash: H256) -> color_eyre::Result<u64, SubxtWrapperError> {
		self.executor.get_block_timestamp(self.node_rpc_url.as_str(), block_hash).await
	}

	async fn fetch_occupied_cores(
		&mut self,
		block_hash: H256,
	) -> color_eyre::Result<Vec<CoreOccupied>, SubxtWrapperError> {
		self.executor.get_occupied_cores(self.node_rpc_url.as_str(), block_hash).await
	}

	async fn read_session_keys(&self, session_index: u32) -> Option<Vec<AccountId32>> {
		let session_hash = BlakeTwo256::hash(&session_index.to_be_bytes()[..]);
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::AccountKeys, session_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	async fn read_inherent_data(&self, block_hash: H256) -> Option<InherentData> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::InherentData, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	async fn read_on_demand_order(&self, block_hash: H256) -> Option<OnDemandOrder> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::OnDemandOrder(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	async fn read_relevant_finalized_block_number(&self, block_hash: H256) -> Option<u32> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::RelevantFinalizedBlockNumber, block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	async fn read_dispute(&self, block_hash: H256) -> Option<DisputeInfo> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::Dispute(self.para_id), block_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	async fn read_candidate(&self, candidate_hash: H256) -> Option<CandidateRecord> {
		self.api
			.storage()
			.storage_read_prefixed(CollectorPrefixType::Candidate(self.para_id), candidate_hash)
			.await
			.map(|v| v.into_inner().unwrap())
	}

	/// Returns the time for the current block
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
		Ok(self.fetch_backing_groups(block_hash).await?.into_iter().flatten().collect())
	}

	async fn candidate_backed_in(&self, candidate_hash: H256) -> Option<u32> {
		self.read_candidate(candidate_hash).await.map(|v| {
			v.candidate_inclusion
				.backed
				.saturating_sub(v.candidate_inclusion.relay_parent_number)
		})
	}
}
