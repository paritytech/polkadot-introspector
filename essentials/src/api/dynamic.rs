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
	types::{CoreOccupied, H256, ValidatorIndex},
};
use subxt::{
	OnlineClient, PolkadotConfig,
	dynamic::Value,
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
	decode_unnamed_composite(raw_groups)?
		.iter()
		.map(|raw_group| {
			decode_unnamed_composite(raw_group)?
				.iter()
				.map(|raw_index| Ok(ValidatorIndex(decode_composite_u128_value(raw_index)? as u32)))
				.collect()
		})
		.collect()
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

pub(crate) fn decode_availability_cores(raw: &Value<u32>) -> Result<Vec<CoreOccupied>, DynamicError> {
	decode_unnamed_composite(raw)?
		.iter()
		.map(|core| match &core.value {
			ValueDef::Variant(variant) => match variant.name.as_str() {
				"Free" => Ok(CoreOccupied::Free),
				"Scheduled" => Ok(CoreOccupied::Scheduled),
				"Occupied" => Ok(CoreOccupied::Occupied),
				other => Err(DynamicError::DecodeDynamicError(
					format!("CoreState variant (Free/Scheduled/Occupied), got '{}'", other),
					core.value.clone(),
				)),
			},
			other => Err(DynamicError::DecodeDynamicError("variant for CoreState".to_string(), other.clone())),
		})
		.collect()
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
