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
//! This is currently a work in progress, but there is a plan in place.
//! At this stage we can only use on-chain data to derive parachain metrics,
//! but later we can expand to use off-chain data as well like gossip.
//!
//! Features:
//! - backing and availability health metrics for all parachains
//! - TODO: backing group information - validator addresses
//! - TODO: parachain block times measured in relay chain blocks
//! - TODO: parachain XCM tput
//! - TODO: parachain code size
//!
//! The CLI interface is useful for debugging/diagnosing issues with the parachain block pipeline.
//! Soon: CI integration also supported via Prometheus metrics exporting.

use crate::core::{
	api::{ApiService, ValidatorIndex},
	EventConsumerInit, RecordsStorageConfig, SubxtEvent,
};

use subxt::sp_runtime::traits::{BlakeTwo256, Hash};

use clap::Parser;
use colored::Colorize;

use log::error;
use std::time::Duration;
use tokio::sync::mpsc::{error::TryRecvError, Receiver};

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct ParachainCommanderOptions {
	/// Websocket url of a relay chain node.
	#[clap(name = "ws", long, value_delimiter = ',', default_value = "wss://rpc.polkadot.io:443")]
	pub node: String,
	/// Parachain id.
	#[clap(long)]
	para_id: u32,
}

pub(crate) struct ParachainCommander {
	opts: ParachainCommanderOptions,
	node: String,
	consumer_config: EventConsumerInit<SubxtEvent>,
	api_service: ApiService,
}

impl ParachainCommander {
	pub(crate) fn new(
		opts: ParachainCommanderOptions,
		consumer_config: EventConsumerInit<SubxtEvent>,
	) -> color_eyre::Result<Self> {
		// This starts the both the storage and subxt APIs.
		let api_service = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1000 });
		let node = opts.node.clone();

		Ok(ParachainCommander { opts, node, consumer_config, api_service })
	}

	// Spawn the UI and subxt tasks and return their futures.
	pub(crate) async fn run(self) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let consumer_channels: Vec<Receiver<SubxtEvent>> = self.consumer_config.into();

		let watcher_future = tokio::spawn(Self::watch_node(
			self.opts.clone(),
			self.node.clone(),
			// There is only one update channel (we only follow one RPC node).
			consumer_channels.into_iter().next().unwrap(),
			self.api_service,
		));

		Ok(vec![watcher_future])
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	async fn watch_node(
		opts: ParachainCommanderOptions,
		url: String,
		mut consumer_config: Receiver<SubxtEvent>,
		api_service: ApiService,
	) {
		// The subxt API request executor.
		let executor = api_service.subxt();
		let para_id = opts.para_id;
		let mut last_assignment = None;
		let mut current_candidate = None;
		let mut last_backed_at = None;
		println!(
			"{} will trace parachain {} on {}",
			"Parachain Commander(TM)".to_string().bold().purple(),
			para_id,
			&url
		);
		println!(
			"{}",
			"-----------------------------------------------------------------------"
				.to_string()
				.bold()
		);

		// Break if user quits.
		loop {
			let recv_result = consumer_config.try_recv();
			match recv_result {
				Ok(event) => {
					if let SubxtEvent::NewHead(hash) = event {
						if let Some(header) = executor.get_block_head(url.clone(), Some(hash)).await {
							if let Some(inherent) = executor.extract_parainherent_data(url.clone(), Some(hash)).await {
								let core_assignments = executor.get_scheduled_paras(url.clone(), hash).await;
								let backed_candidates = inherent.backed_candidates;
								let occupied_cores = executor.get_occupied_cores(url.clone(), hash).await;
								let validator_groups = executor.get_backing_groups(url.clone(), hash).await;

								for candidate in backed_candidates {
									let current_para_id: u32 = candidate.candidate.descriptor.para_id.0;
									if current_para_id == para_id {
										println!(
											"{} parachain {} candidate {} on relay parent {:?}",
											format!("[#{}] BACKED", header.number).bold().green(),
											para_id,
											BlakeTwo256::hash_of(&candidate.candidate),
											header.hash()
										);
										current_candidate = Some(BlakeTwo256::hash_of(&candidate.candidate));
										last_backed_at = Some(header.number);
										break
									}
								}

								for assignment in core_assignments {
									if assignment.para_id.0 == para_id {
										last_assignment = Some(assignment.core.0 as usize);
										println!(
											"\t- Parachain {} assigned to core index {}",
											assignment.para_id.0, assignment.core.0
										);
										break
									}
								}

								if current_candidate.is_none() {
									// Continue if no candidate is backed.
									println!(
										"{} for parachain {}, no candidate backed at {:?}",
										format!("[#{}] SLOW BACKING", header.number).bold().red(),
										para_id,
										header.hash()
									);
									continue
								}

								if let Some(assigned_core) = last_assignment {
									let avail_bits: u32 = inherent
										.bitfields
										.iter()
										.map(|bitfield| {
											let bitfield = &bitfield.payload.0;
											let bit = bitfield[assigned_core];
											bit as u32
										})
										.sum();

									let all_bits =
										validator_groups.into_iter().flatten().collect::<Vec<ValidatorIndex>>();

									// TODO: Availability timeout.
									if avail_bits > (all_bits.len() as u32 / 3) * 2 {
										println!(
											"{} candidate {} in block {:?}",
											format!("[#{}] INCLUDED", header.number).bold().green(),
											current_candidate.unwrap(),
											header.hash()
										);
										current_candidate = None;
									} else if occupied_cores[assigned_core].is_some() &&
										last_backed_at != Some(header.number)
									{
										println!(
											"{} for candidate {} at block {:?}",
											format!("[#{}] SLOW AVAILABILITY", header.number).bold().bright_magenta(),
											current_candidate.unwrap(),
											header.hash()
										);
									}

									println!("\t- Availability bits: {}/{}", avail_bits, all_bits.len());
									println!(
										"\t- Availability core {}",
										if occupied_cores[assigned_core].is_none() { "FREE" } else { "OCCUPIED" }
									);
								}
							} else {
								error!("Cannot get inherent data.")
							}
						} else {
							error!("Failed to get header for {}", hash);
						}
					}
				},
				Err(TryRecvError::Disconnected) => break,
				Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(1000)).await,
			};
		}
	}
}
