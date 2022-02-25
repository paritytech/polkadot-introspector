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

//! Subxt events handlers implementation

use super::{candidate_record::*, records_storage::RecordsStorage};
use crate::{eyre, polkadot};
use log::debug;
use serde::Serialize;
use sp_core::H256;
use sp_runtime::traits::{BlakeTwo256, Hash as CryptoHash};
use std::{
	collections::HashMap,
	error::Error,
	fmt::Debug,
	hash::Hash,
	sync::{Arc, Mutex},
	time::{Duration, SystemTime, UNIX_EPOCH},
};
use subxt::RawEvent;

use typed_builder::TypedBuilder;

// TODO: Convert to a trait as it is a good thing to have a more generic storage
pub type StorageType<T> = Mutex<RecordsStorage<T, CandidateRecord<T>>>;

/// Trait used to update records according to various events
/// Each subxt event is mapped to a corresponding specific CandidateRecordEvent allowing
/// to update candidate state using different subxt events
trait CandidateRecordEvent<T>
where
	T: Hash + Serialize,
{
	type Event;
	type HashType;
	/// Update a record according to a specific event
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>>;
	/// Extract hash from event
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>>;
}

/// Candidate event for a dispute being initiated
impl<T> CandidateRecordEvent<T> for polkadot::paras_disputes::events::DisputeInitiated
where
	T: Hash + Serialize,
{
	type Event = polkadot::paras_disputes::events::DisputeInitiated;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		match record.candidate_disputed {
			None => {
				// Good to go, a new dispute
				record.candidate_disputed =
					Some(CandidateDisputed { disputed: check_unix_time()?, ..Default::default() });
				Ok(())
			},
			Some(_) => Err(format!("duplicate dispute initiated for: {:?}", event.0).into()),
		}
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(event.0 .0.clone())
	}
}

/// Candidate event for a dispute being concluded
impl<T> CandidateRecordEvent<T> for polkadot::paras_disputes::events::DisputeConcluded
where
	T: Hash + Serialize,
{
	type Event = polkadot::paras_disputes::events::DisputeConcluded;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		use polkadot::runtime_types::polkadot_runtime_parachains::disputes::DisputeResult as RuntimeDisputeResult;

		match record.candidate_disputed {
			None => Err(format!("dispute concluded but not initiated: {:?}", event.0).into()),
			Some(ref mut disputed) => match disputed.concluded {
				None => {
					let dispute_result = match event.1 {
						RuntimeDisputeResult::Valid => DisputeOutcome::Agreed,
						_ => DisputeOutcome::Invalid,
					};
					disputed.concluded =
						Some(DisputeResult { concluded_timestamp: check_unix_time()?, outcome: dispute_result });
					Ok(())
				},
				Some(_) => Err(format!("multiple conclusions for a dispute: {:?}", event.0).into()),
			},
		}
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(event.0 .0.clone())
	}
}

/// Candidate event for a dispute being timed out
impl<T> CandidateRecordEvent<T> for polkadot::paras_disputes::events::DisputeTimedOut
where
	T: Hash + Serialize,
{
	type Event = polkadot::paras_disputes::events::DisputeTimedOut;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		match record.candidate_disputed {
			None => Err(format!("dispute concluded but not initiated: {:?}", event.0).into()),
			Some(ref mut disputed) => match disputed.concluded {
				None => {
					disputed.concluded = Some(DisputeResult {
						concluded_timestamp: check_unix_time()?,
						outcome: DisputeOutcome::TimedOut,
					});
					Ok(())
				},
				Some(_) => Err(format!("multiple conclusions for a dispute: {:?}", event.0).into()),
			},
		}
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(event.0 .0.clone())
	}
}

/// Candidate event for candidate being baked for a parachain
impl<T> CandidateRecordEvent<T> for polkadot::para_inclusion::events::CandidateBacked
where
	T: Hash + Serialize,
{
	type Event = polkadot::para_inclusion::events::CandidateBacked;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		record.candidate_inclusion.baked = Some(check_unix_time()?);
		record.candidate_inclusion.core_idx = Some(event.2 .0);
		Ok(())
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(BlakeTwo256::hash_of(&event.0))
	}
}

impl<T> CandidateRecordEvent<T> for polkadot::para_inclusion::events::CandidateIncluded
where
	T: Hash + Serialize,
{
	type Event = polkadot::para_inclusion::events::CandidateIncluded;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		record.candidate_inclusion.included = Some(check_unix_time()?);
		record.candidate_inclusion.core_idx = Some(event.2 .0);
		Ok(())
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(BlakeTwo256::hash_of(&event.0))
	}
}

impl<T> CandidateRecordEvent<T> for polkadot::para_inclusion::events::CandidateTimedOut
where
	T: Hash + Serialize,
{
	type Event = polkadot::para_inclusion::events::CandidateTimedOut;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		record.candidate_inclusion.timedout = Some(check_unix_time()?);
		record.candidate_inclusion.core_idx = Some(event.2 .0);
		Ok(())
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(BlakeTwo256::hash_of(&event.0))
	}
}

pub type EventDecoderFunctor<T> =
	Box<dyn Fn(&RawEvent, &T, Arc<StorageType<T>>) -> Result<(), Box<dyn Error>> + 'static>;

fn gen_handle_event_functor<T, E, F>(decoder: &'static F) -> EventDecoderFunctor<T>
where
	T: Debug + Hash + Serialize + Clone + Eq + 'static,
	E: CandidateRecordEvent<T, Event = E, HashType = T>,
	F: Fn(&RawEvent) -> Result<E, Box<dyn Error>>,
{
	Box::new(move |event, _block, storage| {
		let decoded = decoder(event)?;
		let hash = <E as CandidateRecordEvent<T>>::candidate_hash(&decoded)?;
		let mut unlocked_storage = storage.lock().unwrap();
		let maybe_record = unlocked_storage.get_mut(&hash);
		match maybe_record {
			Some(record) => {
				<E as CandidateRecordEvent<T>>::update_candidate(record, &decoded)?;
			},
			None => {
				let mut record: CandidateRecord<T> = Default::default();
				<E as CandidateRecordEvent<T>>::update_candidate(&mut record, &decoded)?;
				unlocked_storage.insert(hash, record);
			},
		}
		Ok(())
	})
}

/// Struct handler for routes to implement default trait
pub struct EventRouteMap(HashMap<&'static str, HashMap<&'static str, EventDecoderFunctor<H256>>>);

/// Default implementation sets all the routes as expected
impl Default for EventRouteMap {
	fn default() -> Self {
		type EventRoutesMap = HashMap<&'static str, EventDecoderFunctor<H256>>;
		let mut events_by_pallet: HashMap<&'static str, EventRoutesMap> = HashMap::new();

		// TODO: convert this boilerplate to a macro
		let mut disputes_routes: EventRoutesMap = HashMap::new();

		disputes_routes.insert(
			"DisputeInitiated",
			gen_handle_event_functor(&|ev: &RawEvent| -> Result<
				polkadot::paras_disputes::events::DisputeInitiated,
				Box<dyn Error>,
			> {
				<polkadot::paras_disputes::events::DisputeInitiated as codec::Decode>::decode(&mut &ev.data[..])
					.map_err(|e| e.into())
			}),
		);
		disputes_routes.insert(
			"DisputeConcluded",
			gen_handle_event_functor(&|ev: &RawEvent| -> Result<
				polkadot::paras_disputes::events::DisputeConcluded,
				Box<dyn Error>,
			> {
				<polkadot::paras_disputes::events::DisputeConcluded as codec::Decode>::decode(&mut &ev.data[..])
					.map_err(|e| e.into())
			}),
		);
		disputes_routes.insert(
			"DisputeTimedOut",
			gen_handle_event_functor(&|ev: &RawEvent| -> Result<
				polkadot::paras_disputes::events::DisputeTimedOut,
				Box<dyn Error>,
			> {
				<polkadot::paras_disputes::events::DisputeTimedOut as codec::Decode>::decode(&mut &ev.data[..])
					.map_err(|e| e.into())
			}),
		);

		events_by_pallet.insert("ParasDisputes", disputes_routes);

		let mut para_inclusion_routes: EventRoutesMap = HashMap::new();
		para_inclusion_routes.insert(
			"CandidateBacked",
			gen_handle_event_functor(&|ev: &RawEvent| -> Result<
				polkadot::para_inclusion::events::CandidateBacked,
				Box<dyn Error>,
			> {
				<polkadot::para_inclusion::events::CandidateBacked as codec::Decode>::decode(&mut &ev.data[..])
					.map_err(|e| e.into())
			}),
		);
		para_inclusion_routes.insert(
			"CandidateIncluded",
			gen_handle_event_functor(&|ev: &RawEvent| -> Result<
				polkadot::para_inclusion::events::CandidateIncluded,
				Box<dyn Error>,
			> {
				<polkadot::para_inclusion::events::CandidateIncluded as codec::Decode>::decode(&mut &ev.data[..])
					.map_err(|e| e.into())
			}),
		);
		para_inclusion_routes.insert(
			"CandidateTimedOut",
			gen_handle_event_functor(&|ev: &RawEvent| -> Result<
				polkadot::para_inclusion::events::CandidateTimedOut,
				Box<dyn Error>,
			> {
				<polkadot::para_inclusion::events::CandidateTimedOut as codec::Decode>::decode(&mut &ev.data[..])
					.map_err(|e| e.into())
			}),
		);

		events_by_pallet.insert("ParaInclusion", para_inclusion_routes);

		Self(events_by_pallet)
	}
}

/// State for the events handler
#[derive(TypedBuilder)]
pub struct EventsHandler {
	/// Event handlers by pallet
	#[builder(default)]
	pallets: EventRouteMap,
	/// Non generic storage as we are really limited to H256 by subxt so far
	storage: Arc<StorageType<H256>>,
}

impl EventsHandler {
	pub fn handle_runtime_event(&mut self, ev: &RawEvent, block_hash: &H256) -> Result<(), Box<dyn Error>> {
		match self.pallets.0.get_mut(ev.pallet.as_str()) {
			Some(ref mut pallet_handler) => {
				let event_handler = pallet_handler
					.get_mut(ev.variant.as_str())
					.ok_or_else(|| eyre!("Unknown event {} in pallet {}", ev.variant.as_str(), ev.pallet.as_str()))?;
				debug!("Got known raw event: {:?}", ev);
				event_handler(ev, block_hash, self.storage.clone())
			},
			None => {
				debug!("Events for the pallet {} are not handled by introspector", ev.variant.as_str());
				Ok(())
			},
		}
	}
}

fn check_unix_time() -> Result<Duration, Box<dyn Error>> {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_err(|e| eyre!("Clock skewed: {:?}", e).into())
}
