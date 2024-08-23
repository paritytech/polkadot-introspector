// Copyright 2023 Parity Technologies (UK) Ltd.
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

use crate::{
	api::api_client::ApiClient,
	metadata::polkadot_primitives::ValidatorIndex,
	types::{Assignment, BlockNumber, ClaimQueue, CoreOccupied, OnDemandOrder, ParasEntry, H256},
};
use log::error;
use std::collections::{BTreeMap, VecDeque};
use subxt::{
	dynamic::{At, Value},
	ext::scale_value::{Composite, Primitive, ValueDef, Variant},
	OnlineClient, PolkadotConfig,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DynamicError {
	#[error("decode dynamic value error: expected `{0}`, got {1}")]
	DecodeDynamicError(String, ValueDef<u32>),
	#[error("{0} not found in dynamic storage")]
	EmptyResponseFromDynamicStorage(String),
	#[error("subxt error: {0}")]
	SubxtError(#[from] subxt::error::Error),
}

pub(crate) fn decode_validator_groups(raw_groups: &Value<u32>) -> Result<Vec<Vec<ValidatorIndex>>, DynamicError> {
	let decoded_groups = decode_unnamed_composite(raw_groups)?;
	let mut groups = Vec::with_capacity(decoded_groups.len());
	for raw_group in decoded_groups.iter() {
		let decoded_group = decode_unnamed_composite(raw_group)?;
		let mut group = Vec::with_capacity(decoded_group.len());
		for raw_index in decoded_group.iter() {
			group.push(ValidatorIndex(decode_composite_u128_value(raw_index)? as u32));
		}
		groups.push(group)
	}

	Ok(groups)
}

pub(crate) fn decode_availability_cores(raw_cores: &Value<u32>) -> Result<Vec<CoreOccupied>, DynamicError> {
	let decoded_cores = decode_unnamed_composite(raw_cores)?;
	let mut cores = Vec::with_capacity(decoded_cores.len());
	for raw_core in decoded_cores.iter() {
		let core_variant = match decode_option(raw_core) {
			Ok(v) => v,
			// In v5 types it is not more an option
			Err(_) => Some(raw_core),
		};
		let core = match core_variant.map(decode_variant) {
			Some(Ok(variant)) => match variant.name.as_str() {
				"Parachain" => CoreOccupied::Paras, // v4
				"Paras" => CoreOccupied::Paras,     // v5
				"Free" => CoreOccupied::Free,       // v5
				name => todo!("Add support for {name}"),
			},
			Some(Err(e)) => {
				error!("Can't decode a dynamic value: {:?}", e);
				return Err(DynamicError::DecodeDynamicError("core".to_string(), raw_core.value.clone()))
			},
			None => CoreOccupied::Free,
		};
		cores.push(core);
	}

	Ok(cores)
}

pub(crate) fn decode_claim_queue(raw: &Value<u32>) -> Result<ClaimQueue, DynamicError> {
	let decoded_btree_map = decode_unnamed_composite(raw)?;
	let decoded_btree_map_inner = decoded_btree_map
		.first()
		.ok_or(DynamicError::DecodeDynamicError("ClaimQueue".to_string(), raw.value.clone()))?;
	let mut claim_queue: ClaimQueue = BTreeMap::new();
	for value in decode_unnamed_composite(decoded_btree_map_inner)? {
		let (raw_core, raw_para_entries) = match decode_unnamed_composite(value)?[..] {
			[ref first, ref second, ..] => (first, second),
			_ =>
				return Err(DynamicError::DecodeDynamicError("core and paras_entries".to_string(), value.value.clone())),
		};
		let decoded_core = decode_composite_u128_value(raw_core)? as u32;
		let mut paras_entries = VecDeque::new();
		for composite in decode_unnamed_composite(raw_para_entries)? {
			match &composite.value {
				// v5
				ValueDef::Variant(_) => paras_entries.push_back(decode_paras_entry_option(composite)?),
				// v7
				ValueDef::Composite(_) => paras_entries.push_back(Some(decode_paras_entry(composite)?)),
				_ => panic!("No more"),
			};
		}
		let _ = claim_queue.insert(decoded_core, paras_entries);
	}
	Ok(claim_queue)
}

pub(crate) fn decode_on_demand_order(raw: &Composite<u32>) -> Result<OnDemandOrder, DynamicError> {
	match raw {
		Composite::Named(v) => {
			let raw_para_id = v
				.iter()
				.find_map(|(field, value)| if field == "para_id" { Some(value) } else { None })
				.ok_or(DynamicError::DecodeDynamicError(
					"named composite with field `para_id`".to_string(),
					ValueDef::Composite(raw.clone()),
				))?;
			let raw_spot_price = v
				.iter()
				.find_map(|(field, value)| if field == "spot_price" { Some(value) } else { None })
				.ok_or(DynamicError::DecodeDynamicError(
					"named composite with field `spot_price`".to_string(),
					ValueDef::Composite(raw.clone()),
				))?;

			Ok(OnDemandOrder {
				para_id: decode_composite_u128_value(raw_para_id)? as u32,
				spot_price: decode_u128_value(raw_spot_price)?,
			})
		},
		_ => Err(DynamicError::DecodeDynamicError("named composite".to_string(), ValueDef::Composite(raw.clone()))),
	}
}

fn decode_paras_entry_option(raw: &Value<u32>) -> Result<Option<ParasEntry>, DynamicError> {
	match decode_option(raw)? {
		Some(v) => Ok(Some(decode_paras_entry(v)?)),
		None => Ok(None),
	}
}

fn decode_paras_entry(raw: &Value<u32>) -> Result<ParasEntry, DynamicError> {
	let raw_assignment = value_at("assignment", raw)?;
	let para_id = match &raw_assignment.value {
		// v5
		ValueDef::Composite(_) => decode_composite_u128_value(value_at("para_id", raw_assignment)?),
		// v7+
		ValueDef::Variant(_) => match decode_variant(raw_assignment)?.name.as_str() {
			"Bulk" => {
				let raw_para_id = raw_assignment
					.at(0)
					.ok_or(DynamicError::DecodeDynamicError(
						"v7 bulk assignment".to_string(),
						raw_assignment.value.clone(),
					))?
					.at(0)
					.ok_or(DynamicError::DecodeDynamicError(
						"v7 bulk assignment".to_string(),
						raw_assignment.value.clone(),
					))?;
				decode_u128_value(raw_para_id)
			},
			"Pool" => {
				let raw_para_id = value_at("para_id", raw_assignment)?;
				decode_composite_u128_value(raw_para_id)
			},
			_ => Err(DynamicError::DecodeDynamicError("v7 assignment".to_string(), raw_assignment.value.clone())),
		},
		_ => Err(DynamicError::DecodeDynamicError("assignment".to_string(), raw_assignment.value.clone())),
	}? as u32;
	let assignment = Assignment { para_id };
	let availability_timeouts = decode_u128_value(value_at("availability_timeouts", raw)?)? as u32;
	let ttl = decode_u128_value(value_at("ttl", raw)?)? as BlockNumber;

	Ok(ParasEntry { assignment, availability_timeouts, ttl })
}

fn value_at<'a>(field: &'a str, value: &'a Value<u32>) -> Result<&'a Value<u32>, DynamicError> {
	value
		.at(field)
		.ok_or(DynamicError::DecodeDynamicError(format!(".{field}"), value.value.clone()))
}

fn decode_variant(value: &Value<u32>) -> Result<&Variant<u32>, DynamicError> {
	match &value.value {
		ValueDef::Variant(variant) => Ok(variant),
		other => Err(DynamicError::DecodeDynamicError("variant".to_string(), other.clone())),
	}
}

fn decode_option(value: &Value<u32>) -> Result<Option<&Value<u32>>, DynamicError> {
	match decode_variant(value)?.name.as_str() {
		"Some" => Ok(value.at(0)),
		"None" => Ok(None),
		_ => Err(DynamicError::DecodeDynamicError("option".to_string(), value.value.clone())),
	}
}

fn decode_unnamed_composite(value: &Value<u32>) -> Result<&Vec<Value<u32>>, DynamicError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(v)) => Ok(v),
		other => Err(DynamicError::DecodeDynamicError("unnamed composite".to_string(), other.clone())),
	}
}

fn decode_composite_u128_value(value: &Value<u32>) -> Result<u128, DynamicError> {
	match decode_unnamed_composite(value)?[..] {
		[ref first, ..] => decode_u128_value(first),
		_ => Err(DynamicError::DecodeDynamicError("vector of one element".to_string(), value.value.clone())),
	}
}

fn decode_u128_value(value: &Value<u32>) -> Result<u128, DynamicError> {
	match &value.value {
		ValueDef::Primitive(Primitive::U128(v)) => Ok(*v),
		other => Err(DynamicError::DecodeDynamicError("u128".to_string(), other.clone())),
	}
}

pub async fn fetch_dynamic_storage(
	client: &ApiClient<OnlineClient<PolkadotConfig>>,
	maybe_hash: Option<H256>,
	pallet_name: &str,
	entry_name: &str,
) -> std::result::Result<Value<u32>, DynamicError> {
	client
		.fetch_dynamic_storage(maybe_hash, pallet_name, entry_name)
		.await?
		.ok_or(DynamicError::EmptyResponseFromDynamicStorage(format!("{pallet_name}.{entry_name}")))
}

#[derive(Debug)]
pub struct DynamicHostConfiguration(Value<u32>);

impl DynamicHostConfiguration {
	pub fn new(value: Value<u32>) -> Self {
		Self(value)
	}

	pub fn at(&self, field: &str) -> String {
		match self.0.at(field) {
			Some(value) if matches!(value, Value { value: ValueDef::Variant(_), .. }) => match value.at(0) {
				Some(inner) => format!("{}", inner),
				None => format!("{}", 0),
			},
			Some(value) => format!("{}", value),
			None => format!("{}", 0),
		}
	}
}

impl std::fmt::Display for DynamicHostConfiguration {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"\t👀 Max validators: {} / {} per core
\t👍 Needed approvals: {}
\t🥔 No show slots: {}
\t⏳ Delay tranches: {}",
			self.at("max_validators"),
			self.at("max_validators_per_core"),
			self.at("needed_approvals"),
			self.at("no_show_slots"),
			self.at("n_delay_tranches"),
		)
	}
}
