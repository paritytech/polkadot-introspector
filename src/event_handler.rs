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

use crate::candidate_record::CandidateRecord;
use crate::polkadot::{self};
use crate::records_storage::RecordsStorage;

use crate::{eyre, H256};
use sp_runtime::traits::{BlakeTwo256, Hash};
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use subxt::RawEvent;
use typed_builder::TypedBuilder;

// TODO: Convert to a trait as it is a good thing to have a more generic storage
type StorageType<T> = Mutex<RecordsStorage<T, CandidateRecord<T>>>;

/// Trait used to update records according to various events
trait CandidateRecordEvent<T: std::hash::Hash> {
	type Event;
	type HashType;
	/// Update a record according to a specific event
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>>;
	/// Extract hash from event
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>>;
}

impl<T> CandidateRecordEvent<T> for polkadot::paras_disputes::events::DisputeInitiated
where
	T: std::hash::Hash,
{
	type Event = polkadot::paras_disputes::events::DisputeInitiated;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		Ok(())
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(event.0 .0.clone())
	}
}

impl<T> CandidateRecordEvent<T> for polkadot::paras_disputes::events::DisputeConcluded
where
	T: std::hash::Hash,
{
	type Event = polkadot::paras_disputes::events::DisputeConcluded;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		Ok(())
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(event.0 .0.clone())
	}
}

impl<T> CandidateRecordEvent<T> for polkadot::paras_disputes::events::DisputeTimedOut
where
	T: std::hash::Hash,
{
	type Event = polkadot::paras_disputes::events::DisputeTimedOut;
	type HashType = H256;
	fn update_candidate(record: &mut CandidateRecord<T>, event: &Self::Event) -> Result<(), Box<dyn Error>> {
		Ok(())
	}
	fn candidate_hash(event: &Self::Event) -> Result<Self::HashType, Box<dyn Error>> {
		Ok(event.0 .0.clone())
	}
}

//pub type EventDecoder<E> = dyn Fn(&RawEvent) -> Result<E, Box<dyn Error>> + 'static;
pub type EventDecoderFunctor<T> =
	Box<dyn Fn(&RawEvent, &T, Arc<StorageType<T>>) -> Result<(), Box<dyn Error>> + 'static>;

fn gen_handle_event_functor<T, E, F>(decoder: &'static F) -> EventDecoderFunctor<T>
where
	T: Debug + std::hash::Hash + Clone + Eq + 'static,
	E: CandidateRecordEvent<T, Event = E, HashType = T>,
	F: Fn(&RawEvent) -> Result<E, Box<dyn Error>>,
{
	Box::new(move |event, block, storage| {
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

impl Default for EventRouteMap {
	fn default() -> Self {
		type EventRoutesMap = HashMap<&'static str, EventDecoderFunctor<H256>>;
		let mut events_by_pallet: HashMap<&'static str, EventRoutesMap> = HashMap::new();

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
		let pallet = self
			.pallets
			.0
			.get_mut(ev.pallet.as_str())
			.ok_or(eyre!("Unknown pallet: {}", ev.pallet.as_str()))?;
		let event_handler = pallet.get_mut(ev.variant.as_str()).ok_or(eyre!(
			"Unknown event {} in pallet {}",
			ev.variant.as_str(),
			ev.pallet.as_str()
		))?;

		event_handler(ev, block_hash, self.storage.clone())
	}
}
