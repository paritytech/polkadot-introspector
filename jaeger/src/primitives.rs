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
//

//! Jaeger tracing primitives

use serde::{Deserialize, Serialize, de::Deserializer};
use std::collections::HashMap;

/// RPC Primitives
#[derive(Serialize, Deserialize, Debug)]
pub struct RpcResponse<T> {
	data: Vec<T>,
	total: usize,
	limit: usize,
	offset: usize,
	errors: Option<serde_json::Value>,
}

impl<T> RpcResponse<T> {
	pub fn consume(self) -> Vec<T> {
		self.data
	}
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TraceObject<'a> {
	#[serde(rename = "traceID")]
	trace_id: &'a str,
	#[serde(deserialize_with = "deserialize_spans_as_hashmap")]
	pub spans: HashMap<&'a str, Span<'a>>,
	#[serde(borrow)]
	processes: HashMap<&'a str, Process<'a>>,
	warnings: Option<Vec<&'a str>>,
}

fn deserialize_spans_as_hashmap<'de, D>(deserializer: D) -> Result<HashMap<&'de str, Span<'de>>, D::Error>
where
	D: Deserializer<'de>,
{
	let vec_input = Vec::<Span<'de>>::deserialize(deserializer)?;
	let mut map = HashMap::with_capacity(vec_input.len());

	for item in vec_input.into_iter() {
		map.insert(item.span_id, item);
	}
	Ok(map)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Span<'a> {
	#[serde(rename = "traceID")]
	pub trace_id: &'a str,
	#[serde(rename = "spanID")]
	pub span_id: &'a str,
	pub flags: Option<usize>,
	#[serde(rename = "operationName")]
	pub operation_name: &'a str,
	#[serde(borrow)]
	pub references: Vec<Reference<'a>>,
	#[serde(rename = "startTime")]
	pub start_time: usize,
	pub duration: f64,
	#[serde(borrow)]
	pub tags: Vec<Tag<'a>>,
	pub logs: Vec<serde_json::Value>, // FIXME: not sure what an actual 'log' looks like
	#[serde(rename = "processID")]
	pub process_id: &'a str,
	#[serde(borrow)]
	pub warnings: Option<Vec<&'a str>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Tag<'a> {
	key: &'a str,
	#[serde(rename = "type")]
	ty: &'a str,
	#[serde(borrow)]
	value: TagValue<'a>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum TagValue<'a> {
	String(&'a str),
	Boolean(bool),
	Number(usize),
}

impl std::fmt::Display for TagValue<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		match self {
			TagValue::String(s) => write!(f, "{}", s),
			TagValue::Boolean(b) => write!(f, "{}", b),
			TagValue::Number(n) => write!(f, "{}", n),
		}
	}
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Process<'a> {
	#[serde(rename = "serviceName")]
	service_name: &'a str,
	#[serde(borrow)]
	tags: Vec<Tag<'a>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Reference<'a> {
	#[serde(rename = "refType")]
	ref_type: &'a str,
	#[serde(rename = "traceID")]
	trace_id: &'a str,
	#[serde(rename = "spanID")]
	span_id: &'a str,
}
