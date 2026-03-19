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
	metadata::{
		polkadot::runtime_types::polkadot_core_primitives::CandidateHash,
		polkadot_primitives::{
			AvailabilityBitfield, DisputeStatement, InvalidDisputeStatementKind, ValidDisputeStatementKind,
			ValidatorIndex,
		},
	},
	types::{CoreOccupied, DisputeStatementSet, H256, InherentData, OnDemandOrder},
};
use subxt::{
	OnlineClient, PolkadotConfig,
	dynamic::{At, Value},
	ext::scale_value::{Composite, Primitive, ValueDef, Variant},
	utils::bits::DecodedBits,
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
		ValueDef::Primitive(Primitive::U128(v)) => u32::try_from(*v).map_err(|_| {
			DynamicError::DecodeDynamicError(format!("u32 value (got {} which overflows u32)", v), value.value.clone())
		}),
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

fn decode_byte_slice(bytes: &[Value<u32>]) -> Result<Vec<u8>, DynamicError> {
	bytes
		.iter()
		.enumerate()
		.map(|(i, b)| match &b.value {
			ValueDef::Primitive(Primitive::U128(v)) => u8::try_from(*v).map_err(|_| {
				DynamicError::DecodeDynamicError(
					format!("u8 at index {} (got {} which overflows u8)", i, v),
					b.value.clone(),
				)
			}),
			other => Err(DynamicError::DecodeDynamicError(format!("u8 at index {}", i), other.clone())),
		})
		.collect()
}

fn decode_h256_from_bytes(bytes: &[Value<u32>]) -> Result<H256, DynamicError> {
	let decoded = decode_byte_slice(bytes)?;
	let arr: [u8; 32] = decoded.try_into().map_err(|v: Vec<u8>| {
		DynamicError::DecodeDynamicError(
			format!("32 bytes (got {})", v.len()),
			ValueDef::Composite(Composite::Unnamed(bytes.to_vec())),
		)
	})?;
	Ok(H256::from(arr))
}

fn find_named_field<'a>(
	fields: &'a [(String, Value<u32>)],
	name: &str,
	context: &Composite<u32>,
) -> Result<&'a Value<u32>, DynamicError> {
	fields
		.iter()
		.find_map(|(field, value)| if field == name { Some(value) } else { None })
		.ok_or_else(|| {
			DynamicError::DecodeDynamicError(format!("field '{}'", name), ValueDef::Composite(context.clone()))
		})
}

fn variant_inner_value<'a>(variant: &'a Variant<u32>, context: &str) -> Result<&'a Value<u32>, DynamicError> {
	variant.values.values().next().ok_or_else(|| {
		DynamicError::DecodeDynamicError(format!("inner value for {}", context), ValueDef::Variant(variant.clone()))
	})
}

pub(crate) fn decode_on_demand_order(raw: &Composite<u32>) -> Result<OnDemandOrder, DynamicError> {
	match raw {
		Composite::Named(v) => {
			let raw_para_id = find_named_field(v, "para_id", raw)?;
			let raw_spot_price = find_named_field(v, "spot_price", raw)?;
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

pub(crate) fn decode_inherent_data(raw: &Composite<u32>) -> Result<InherentData, DynamicError> {
	let data = match raw {
		Composite::Named(fields) => find_named_field(fields, "data", raw)?,
		Composite::Unnamed(fields) if !fields.is_empty() => &fields[0],
		_ =>
			return Err(DynamicError::DecodeDynamicError(
				"composite with inherent data".to_string(),
				ValueDef::Composite(raw.clone()),
			)),
	};

	let raw_bitfields = data
		.at("bitfields")
		.ok_or_else(|| DynamicError::DecodeDynamicError("bitfields field".to_string(), data.value.clone()))?;
	let bitfields: Result<Vec<_>, _> = decode_unnamed_composite(raw_bitfields)?
		.iter()
		.map(|raw_signed| {
			let payload = raw_signed.at("payload").ok_or_else(|| {
				DynamicError::DecodeDynamicError("payload field in bitfield".to_string(), raw_signed.value.clone())
			})?;
			decode_availability_bitfield(payload)
		})
		.collect();

	let raw_disputes = data
		.at("disputes")
		.ok_or_else(|| DynamicError::DecodeDynamicError("disputes field".to_string(), data.value.clone()))?;
	let disputes: Result<Vec<_>, _> = decode_unnamed_composite(raw_disputes)?
		.iter()
		.map(decode_dispute_statement_set)
		.collect();

	Ok(InherentData { bitfields: bitfields?, disputes: disputes? })
}

fn decode_availability_bitfield(value: &Value<u32>) -> Result<AvailabilityBitfield, DynamicError> {
	match &value.value {
		ValueDef::BitSequence(bits) => Ok(AvailabilityBitfield(DecodedBits::from_iter(bits.iter()))),
		ValueDef::Composite(Composite::Unnamed(inner)) if inner.len() == 1 => decode_availability_bitfield(&inner[0]),
		ValueDef::Composite(Composite::Unnamed(bits)) => {
			let bools: Result<Vec<bool>, _> = bits
				.iter()
				.map(|b| match &b.value {
					ValueDef::Primitive(Primitive::Bool(v)) => Ok(*v),
					ValueDef::Primitive(Primitive::U128(v)) => Ok(*v != 0),
					other => Err(DynamicError::DecodeDynamicError("bool in bitfield".to_string(), other.clone())),
				})
				.collect();
			Ok(AvailabilityBitfield(DecodedBits::from_iter(bools?)))
		},
		other => Err(DynamicError::DecodeDynamicError("availability bitfield".to_string(), other.clone())),
	}
}

fn decode_dispute_statement_set(value: &Value<u32>) -> Result<DisputeStatementSet, DynamicError> {
	let raw_candidate_hash = value
		.at("candidate_hash")
		.ok_or_else(|| DynamicError::DecodeDynamicError("candidate_hash field".to_string(), value.value.clone()))?;
	let candidate_hash = decode_candidate_hash(raw_candidate_hash)?;

	let raw_session = value
		.at("session")
		.ok_or_else(|| DynamicError::DecodeDynamicError("session field".to_string(), value.value.clone()))?;
	let session = decode_dynamic_u32(raw_session)?;

	let raw_statements = value
		.at("statements")
		.ok_or_else(|| DynamicError::DecodeDynamicError("statements field".to_string(), value.value.clone()))?;
	let statements_vec = decode_unnamed_composite(raw_statements)?;
	let mut statements = Vec::with_capacity(statements_vec.len());
	for raw_stmt in statements_vec {
		let tuple = decode_unnamed_composite(raw_stmt)?;
		if tuple.len() < 3 {
			return Err(DynamicError::DecodeDynamicError(
				"statement tuple with 3 elements".to_string(),
				raw_stmt.value.clone(),
			));
		}
		let statement = decode_dispute_statement(&tuple[0])?;
		let validator_index = ValidatorIndex(decode_dynamic_u32(&tuple[1])?);
		statements.push((statement, validator_index));
	}

	Ok(DisputeStatementSet { candidate_hash, session, statements })
}

fn decode_dispute_statement(value: &Value<u32>) -> Result<DisputeStatement, DynamicError> {
	match &value.value {
		ValueDef::Variant(variant) => match variant.name.as_str() {
			"Valid" => Ok(DisputeStatement::Valid(decode_valid_dispute_statement_kind(variant)?)),
			"Invalid" => Ok(DisputeStatement::Invalid(decode_invalid_dispute_statement_kind(variant)?)),
			other => Err(DynamicError::DecodeDynamicError(
				format!("Valid or Invalid dispute statement, got '{}'", other),
				value.value.clone(),
			)),
		},
		other => Err(DynamicError::DecodeDynamicError("variant for DisputeStatement".to_string(), other.clone())),
	}
}

fn decode_valid_dispute_statement_kind(variant: &Variant<u32>) -> Result<ValidDisputeStatementKind, DynamicError> {
	let inner = variant_inner_value(variant, "ValidDisputeStatementKind")?;
	match &inner.value {
		ValueDef::Variant(inner_variant) => match inner_variant.name.as_str() {
			"Explicit" => Ok(ValidDisputeStatementKind::Explicit),
			"BackingSeconded" => {
				let hash_val = variant_inner_value(inner_variant, "BackingSeconded")?;
				Ok(ValidDisputeStatementKind::BackingSeconded(decode_h256(hash_val)?))
			},
			"BackingValid" => {
				let hash_val = variant_inner_value(inner_variant, "BackingValid")?;
				Ok(ValidDisputeStatementKind::BackingValid(decode_h256(hash_val)?))
			},
			"ApprovalChecking" => Ok(ValidDisputeStatementKind::ApprovalChecking),
			"ApprovalCheckingMultipleCandidates" => {
				let candidates_val = variant_inner_value(inner_variant, "ApprovalCheckingMultipleCandidates")?;
				let candidates_vec = decode_unnamed_composite(candidates_val)?;
				let candidates: Result<Vec<_>, _> = candidates_vec.iter().map(decode_candidate_hash).collect();
				Ok(ValidDisputeStatementKind::ApprovalCheckingMultipleCandidates(candidates?))
			},
			other => Err(DynamicError::DecodeDynamicError(
				format!("known ValidDisputeStatementKind, got '{}'", other),
				inner.value.clone(),
			)),
		},
		_ => Err(DynamicError::DecodeDynamicError(
			"variant for ValidDisputeStatementKind".to_string(),
			inner.value.clone(),
		)),
	}
}

fn decode_invalid_dispute_statement_kind(variant: &Variant<u32>) -> Result<InvalidDisputeStatementKind, DynamicError> {
	let inner = variant_inner_value(variant, "InvalidDisputeStatementKind")?;
	match &inner.value {
		ValueDef::Variant(inner_variant) => match inner_variant.name.as_str() {
			"Explicit" => Ok(InvalidDisputeStatementKind::Explicit),
			other => Err(DynamicError::DecodeDynamicError(
				format!("known InvalidDisputeStatementKind, got '{}'", other),
				inner.value.clone(),
			)),
		},
		_ => Err(DynamicError::DecodeDynamicError(
			"variant for InvalidDisputeStatementKind".to_string(),
			inner.value.clone(),
		)),
	}
}

fn decode_candidate_hash(value: &Value<u32>) -> Result<CandidateHash, DynamicError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(inner)) if inner.len() == 1 =>
			Ok(CandidateHash(decode_h256(&inner[0])?)),
		ValueDef::Composite(Composite::Named(fields)) => {
			let hash_val = fields
				.iter()
				.find_map(|(name, val)| if name == "0" { Some(val) } else { None })
				.ok_or_else(|| {
					DynamicError::DecodeDynamicError(
						"field '0' in named CandidateHash composite".to_string(),
						value.value.clone(),
					)
				})?;
			Ok(CandidateHash(decode_h256(hash_val)?))
		},
		other => Err(DynamicError::DecodeDynamicError(
			"CandidateHash (unnamed 1-tuple or named composite with field '0')".to_string(),
			other.clone(),
		)),
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
