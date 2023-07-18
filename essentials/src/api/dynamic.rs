use super::subxt_wrapper::SubxtWrapperError::{self, DecodeDynamicError};
use crate::metadata::polkadot_primitives;
use subxt::{
	dynamic::{At, Value},
	ext::scale_value::{Composite, Primitive, ValueDef},
};

pub(crate) fn decode_dynamic_validator_groups(
	raw_groups: &Value<u32>,
) -> Result<Vec<Vec<polkadot_primitives::ValidatorIndex>>, SubxtWrapperError> {
	let decoded_groups = decode_vector(raw_groups)?;
	let mut groups = vec![];
	for raw_group in decoded_groups.iter() {
		let decoded_group = decode_vector(raw_group)?;
		let mut group = vec![];
		for raw_index in decoded_group.iter() {
			group.push(decode_validator_index_value(raw_index)?)
		}
		groups.push(group)
	}

	Ok(groups)
}

fn decode_vector(value: &Value<u32>) -> Result<&Vec<Value<u32>>, SubxtWrapperError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(v)) => Ok(v),
		other => return Err(DecodeDynamicError("vector".to_string(), other.clone())),
	}
}

fn decode_option(value: &Value<u32>) -> Result<Option<&Value<u32>>, SubxtWrapperError> {
	if matches!(value, Value { value: ValueDef::Variant(_), .. }) {
		Ok(value.at(0))
	} else {
		Err(DecodeDynamicError("vector of validator indices".to_string(), value.value.clone()))
	}
}

fn decode_validator_index_value(value: &Value<u32>) -> Result<polkadot_primitives::ValidatorIndex, SubxtWrapperError> {
	match &decode_vector(value)?.first().expect("Expected a vector of one").value {
		ValueDef::Primitive(Primitive::U128(index)) => Ok(polkadot_primitives::ValidatorIndex(*index as u32)),
		other => Err(DecodeDynamicError("validator's index".to_string(), other.clone())),
	}
}

pub(crate) fn decode_dynamic_availability_cores(
	raw_cores: &Value<u32>,
) -> Result<Vec<Option<polkadot_primitives::CoreOccupied>>, SubxtWrapperError> {
	let decoded_cores = decode_vector(raw_cores)?;
	let mut cores = vec![];
	for row_core in decoded_cores.iter() {
		cores.push(decode_core_occupied(row_core)?);
	}

	Ok(cores)
}

// TODO: Add support for on demand parachains
fn decode_core_occupied(value: &Value<u32>) -> Result<Option<polkadot_primitives::CoreOccupied>, SubxtWrapperError> {
	Ok(decode_option(value)?.map(|_| polkadot_primitives::CoreOccupied::Parachain))
}
