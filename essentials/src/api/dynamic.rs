use super::subxt_wrapper::SubxtWrapperError::{self, DecodeDynamicError};
use crate::{
	metadata::{
		polkadot::runtime_types::{
			polkadot_parachain::primitives::Id,
			polkadot_runtime_parachains::scheduler::{AssignmentKind, CoreAssignment},
		},
		polkadot_primitives::{CoreIndex, GroupIndex, ValidatorIndex},
	},
	types::{Assignment, BlockNumber, ClaimQueue, CoreOccupied, ParasEntry},
};
use log::error;
use std::collections::{BTreeMap, VecDeque};
use subxt::{
	dynamic::{At, Value},
	ext::scale_value::{Composite, Primitive, ValueDef, Variant},
};

pub(crate) fn decode_dynamic_validator_groups(
	raw_groups: &Value<u32>,
) -> Result<Vec<Vec<ValidatorIndex>>, SubxtWrapperError> {
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

pub(crate) fn decode_dynamic_availability_cores(
	raw_cores: &Value<u32>,
) -> Result<Vec<CoreOccupied>, SubxtWrapperError> {
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
				return Err(DecodeDynamicError("core".to_string(), raw_core.value.clone()))
			},
			None => CoreOccupied::Free,
		};
		cores.push(core);
	}

	Ok(cores)
}

pub(crate) fn decode_dynamic_scheduled_paras(raw_paras: &Value<u32>) -> Result<Vec<CoreAssignment>, SubxtWrapperError> {
	let decoded_paras = decode_unnamed_composite(raw_paras)?;
	let mut paras = Vec::with_capacity(decoded_paras.len());
	for para in decoded_paras.iter() {
		let core = CoreIndex(decode_composite_u128_value(value_at("core", para)?)? as u32);
		let para_id = Id(decode_composite_u128_value(value_at("para_id", para)?)? as u32);
		let kind = match decode_variant(value_at("kind", para)?)?.name.as_str() {
			"Parachain" => AssignmentKind::Parachain,
			name => todo!("Add support for {name}"),
		};
		let group_idx = GroupIndex(decode_composite_u128_value(value_at("group_idx", para)?)? as u32);
		let assignment = CoreAssignment { core, para_id, kind, group_idx };

		paras.push(assignment)
	}

	Ok(paras)
}

pub(crate) fn decode_dynamic_claim_queue(raw: &Value<u32>) -> Result<ClaimQueue, SubxtWrapperError> {
	let decoded_btree_map = decode_unnamed_composite(raw)?;
	let decoded_btree_map_inner = decoded_btree_map
		.first()
		.ok_or(SubxtWrapperError::DecodeDynamicError("ClaimQueue".to_string(), raw.value.clone()))?;
	let mut claim_queue: ClaimQueue = BTreeMap::new();
	for value in decode_unnamed_composite(decoded_btree_map_inner)? {
		let (raw_core, raw_para_entries) = match decode_unnamed_composite(value)?[..] {
			[ref first, ref second, ..] => (first, second),
			_ =>
				return Err(SubxtWrapperError::DecodeDynamicError(
					"core and paras_entries".to_string(),
					value.value.clone(),
				)),
		};
		let decoded_core = decode_composite_u128_value(raw_core)? as u32;
		let mut paras_entries = VecDeque::new();
		for composite in decode_unnamed_composite(raw_para_entries)? {
			paras_entries.push_back(decode_paras_entry_option(composite)?)
		}
		let _ = claim_queue.insert(decoded_core, paras_entries);
	}
	Ok(claim_queue)
}

fn decode_paras_entry_option(raw: &Value<u32>) -> Result<Option<ParasEntry>, SubxtWrapperError> {
	match decode_option(raw)? {
		Some(v) => Ok(Some(decode_paras_entry(v)?)),
		None => Ok(None),
	}
}

fn decode_paras_entry(raw: &Value<u32>) -> Result<ParasEntry, SubxtWrapperError> {
	let para_id = decode_composite_u128_value(value_at("para_id", value_at("assignment", raw)?)?)? as u32;
	let assignment = Assignment { para_id };
	let retries = decode_u128_value(value_at("retries", raw)?)? as u32;
	let ttl = decode_u128_value(value_at("ttl", raw)?)? as BlockNumber;

	Ok(ParasEntry { assignment, retries, ttl })
}

fn value_at<'a>(field: &'a str, value: &'a Value<u32>) -> Result<&'a Value<u32>, SubxtWrapperError> {
	value
		.at(field)
		.ok_or(DecodeDynamicError(format!(".{field}"), value.value.clone()))
}

fn decode_variant(value: &Value<u32>) -> Result<&Variant<u32>, SubxtWrapperError> {
	match &value.value {
		ValueDef::Variant(variant) => Ok(variant),
		other => Err(DecodeDynamicError("variant".to_string(), other.clone())),
	}
}

fn decode_option(value: &Value<u32>) -> Result<Option<&Value<u32>>, SubxtWrapperError> {
	match decode_variant(value)?.name.as_str() {
		"Some" => Ok(value.at(0)),
		"None" => Ok(None),
		_ => Err(DecodeDynamicError("option".to_string(), value.value.clone())),
	}
}

fn decode_unnamed_composite(value: &Value<u32>) -> Result<&Vec<Value<u32>>, SubxtWrapperError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(v)) => Ok(v),
		other => Err(DecodeDynamicError("unnamed composite".to_string(), other.clone())),
	}
}

fn decode_composite_u128_value(value: &Value<u32>) -> Result<u128, SubxtWrapperError> {
	match decode_unnamed_composite(value)?[..] {
		[ref first, ..] => decode_u128_value(first),
		_ => Err(DecodeDynamicError("vector of one element".to_string(), value.value.clone())),
	}
}

fn decode_u128_value(value: &Value<u32>) -> Result<u128, SubxtWrapperError> {
	match &value.value {
		ValueDef::Primitive(Primitive::U128(v)) => Ok(*v),
		other => Err(DecodeDynamicError("u128".to_string(), other.clone())),
	}
}
