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
	types::{H256, OnDemandOrder},
};
use subxt::{
	OnlineClient, PolkadotConfig,
	dynamic::{At, Value},
	ext::scale_value::{Composite, Primitive, ValueDef},
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DynamicError {
	#[error("decode dynamic value error: expected `{0}`, got {1}")]
	DecodeDynamicError(String, ValueDef<u32>),
	#[error("{0} not found in dynamic storage")]
	EmptyResponseFromDynamicStorage(String),
	#[error("subxt error: {0}")]
	SubxtError(String),
}

impl From<subxt::error::Error> for DynamicError {
	fn from(err: subxt::error::Error) -> Self {
		Self::SubxtError(err.to_string())
	}
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

pub(crate) fn decode_candidate_event(raw: &Composite<u32>) -> Result<(u32, H256, u32), DynamicError> {
	let receipt = match raw {
		Composite::Unnamed(fields) if !fields.is_empty() => &fields[0],
		_ =>
			return Err(DynamicError::DecodeDynamicError(
				"unnamed composite with candidate receipt".to_string(),
				ValueDef::Composite(raw.clone()),
			)),
	};

	let descriptor = receipt
		.at("descriptor")
		.ok_or_else(|| DynamicError::DecodeDynamicError("descriptor field".to_string(), receipt.value.clone()))?;

	let raw_para_id = descriptor
		.at("para_id")
		.ok_or_else(|| DynamicError::DecodeDynamicError("para_id field".to_string(), descriptor.value.clone()))?;
	let para_id = decode_dynamic_u32(raw_para_id)?;

	let raw_relay_parent = descriptor
		.at("relay_parent")
		.ok_or_else(|| DynamicError::DecodeDynamicError("relay_parent field".to_string(), descriptor.value.clone()))?;
	let relay_parent = decode_h256(raw_relay_parent)?;

	let core_idx = match raw {
		Composite::Unnamed(fields) if fields.len() > 2 => decode_dynamic_u32(&fields[2])?,
		_ =>
			return Err(DynamicError::DecodeDynamicError(
				"core_index field (3rd unnamed field)".to_string(),
				ValueDef::Composite(raw.clone()),
			)),
	};

	Ok((para_id, relay_parent, core_idx))
}

fn decode_dynamic_u32(value: &Value<u32>) -> Result<u32, DynamicError> {
	match &value.value {
		ValueDef::Primitive(Primitive::U128(v)) => Ok(*v as u32),
		ValueDef::Composite(Composite::Unnamed(inner)) if !inner.is_empty() => decode_dynamic_u32(&inner[0]),
		other => Err(DynamicError::DecodeDynamicError("u32-like value".to_string(), other.clone())),
	}
}

fn decode_h256(value: &Value<u32>) -> Result<H256, DynamicError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(bytes)) if bytes.len() == 32 => decode_h256_from_bytes(bytes),
		ValueDef::Composite(Composite::Unnamed(inner)) if inner.len() == 1 => decode_h256(&inner[0]),
		other => Err(DynamicError::DecodeDynamicError("H256 (32-byte composite)".to_string(), other.clone())),
	}
}

fn decode_h256_from_bytes(bytes: &[Value<u32>]) -> Result<H256, DynamicError> {
	let mut arr = [0u8; 32];
	for (i, b) in bytes.iter().enumerate() {
		match &b.value {
			ValueDef::Primitive(Primitive::U128(v)) => arr[i] = *v as u8,
			other => return Err(DynamicError::DecodeDynamicError(format!("u8 at index {}", i), other.clone())),
		}
	}
	Ok(H256::from(arr))
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
