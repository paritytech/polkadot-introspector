use super::subxt_wrapper::SubxtWrapperError::{self, DecodeDynamicError};
use crate::metadata::polkadot_primitives;
use subxt::{
	dynamic::Value,
	ext::scale_value::{Composite, Primitive, ValueDef},
};

pub(crate) fn decode_dynamic_validator_groups(
	raw_groups: &Value<u32>,
) -> Result<Vec<Vec<polkadot_primitives::ValidatorIndex>>, SubxtWrapperError> {
	let decoded_groups = decode_validator_groups_value(raw_groups)?;
	let mut groups = vec![];
	for raw_group in decoded_groups.iter() {
		let decoded_group = decode_validator_group_value(raw_group)?;
		let mut group = vec![];
		for raw_index in decoded_group.iter() {
			group.push(decode_validator_index_value(raw_index)?)
		}
		groups.push(group)
	}

	Ok(groups)
}

fn decode_validator_groups_value(value: &Value<u32>) -> Result<&Vec<Value<u32>>, SubxtWrapperError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(v)) => Ok(v),
		other => return Err(DecodeDynamicError("vector of validator groups".to_string(), other.clone())),
	}
}

fn decode_validator_group_value(value: &Value<u32>) -> Result<&Vec<Value<u32>>, SubxtWrapperError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(v)) => Ok(v),
		other => Err(DecodeDynamicError("vector of validator indices".to_string(), other.clone())),
	}
}

fn decode_validator_index_value(value: &Value<u32>) -> Result<polkadot_primitives::ValidatorIndex, SubxtWrapperError> {
	match &value.value {
		ValueDef::Composite(Composite::Unnamed(v)) => match &v.first().expect("Expected a vector of one").value {
			ValueDef::Primitive(Primitive::U128(index)) => Ok(polkadot_primitives::ValidatorIndex(*index as u32)),
			other => Err(DecodeDynamicError("validator's index".to_string(), other.clone())),
		},
		other => Err(DecodeDynamicError("vector with validator's index".to_string(), other.clone())),
	}
}
