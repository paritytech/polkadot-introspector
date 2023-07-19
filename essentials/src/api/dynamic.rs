use super::subxt_wrapper::SubxtWrapperError::{self, DecodeDynamicError};
use crate::metadata::{
	polkadot::runtime_types::{
		polkadot_parachain::primitives::Id,
		polkadot_runtime_parachains::scheduler::{AssignmentKind, CoreAssignment},
	},
	polkadot_primitives::{CoreIndex, CoreOccupied, GroupIndex, ValidatorIndex},
};
use subxt::{
	dynamic::{At, Value},
	ext::scale_value::{Composite, Primitive, ValueDef, Variant},
};

pub(crate) fn decode_dynamic_validator_groups(
	raw_groups: &Value<u32>,
) -> Result<Vec<Vec<ValidatorIndex>>, SubxtWrapperError> {
	let decoded_groups = decode_vector(raw_groups)?;
	let mut groups = Vec::with_capacity(decoded_groups.len());
	for raw_group in decoded_groups.iter() {
		let decoded_group = decode_vector(raw_group)?;
		let mut group = Vec::with_capacity(decoded_group.len());
		for raw_index in decoded_group.iter() {
			group.push(ValidatorIndex(decode_u128_value(raw_index)? as u32));
		}
		groups.push(group)
	}

	Ok(groups)
}

pub(crate) fn decode_dynamic_availability_cores(
	raw_cores: &Value<u32>,
) -> Result<Vec<Option<CoreOccupied>>, SubxtWrapperError> {
	let decoded_cores = decode_vector(raw_cores)?;
	let mut cores = Vec::with_capacity(decoded_cores.len());
	for raw_core in decoded_cores.iter() {
		cores.push(decode_option(raw_core)?.map(|v| match decode_variant(v).unwrap().name.as_str() {
			"Parachain" => CoreOccupied::Parachain,
			_ => todo!("Add parathreads support"),
		}));
	}

	Ok(cores)
}

pub(crate) fn decode_dynamic_scheduled_paras(raw_paras: &Value<u32>) -> Result<Vec<CoreAssignment>, SubxtWrapperError> {
	let decoded_paras = decode_vector(raw_paras)?;
	let mut paras = Vec::with_capacity(decoded_paras.len());
	for para in decoded_paras.iter() {
		let core = CoreIndex(decode_u128_value(para.at("core").expect("Should be defined"))? as u32);
		let para_id = Id(decode_u128_value(para.at("para_id").expect("Should be defined"))? as u32);
		let kind = match decode_variant(para.at("kind").expect("Should be defined"))?.name.as_str() {
			"Parachain" => AssignmentKind::Parachain,
			_ => todo!("Add parathreads support"),
		};
		let group_idx = GroupIndex(decode_u128_value(para.at("group_idx").expect("Should be defined"))? as u32);
		let assignment = CoreAssignment { core, para_id, kind, group_idx };

		paras.push(assignment)
	}

	Ok(paras)
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

fn decode_vector(value: &Value<u32>) -> Result<&Vec<Value<u32>>, SubxtWrapperError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(v)) => Ok(v),
		other => Err(DecodeDynamicError("vector".to_string(), other.clone())),
	}
}

fn decode_u128_value(value: &Value<u32>) -> Result<u128, SubxtWrapperError> {
	match &decode_vector(value)?.first().expect("a vector of one").value {
		ValueDef::Primitive(Primitive::U128(v)) => Ok(*v),
		other => Err(DecodeDynamicError("u128".to_string(), other.clone())),
	}
}
