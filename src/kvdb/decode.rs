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

//! KVDB decoding functions

use crate::kvdb::IntrospectorKvdb;
use color_eyre::{eyre::eyre, Result};
use erased_serde::{serialize_trait_object, Serialize};
use itertools::Itertools;
use std::fmt::Debug;
use subxt::sp_core::H256;

/// Decode result trait, used to display and format output of the decoder
pub trait DecodeResult: Debug + Serialize {}
serialize_trait_object!(DecodeResult);

impl DecodeResult for i32 {}
impl DecodeResult for u64 {}
impl DecodeResult for String {}

type DecoderFunctor = Box<dyn Fn(&[u8]) -> Result<Box<dyn DecodeResult>>>;

/// This structure is used to decode a single entry
pub struct DecodeElement {
	/// Represents a decoder functor
	decoder: DecoderFunctor,
	/// How much bytes this functor consume
	consume_size: usize,
}

// Parses a single i32 in big endian order
fn consume_bigendian_i32() -> DecodeElement {
	let decoder = Box::new(|input: &[u8]| -> Result<Box<dyn DecodeResult>> {
		let (int_bytes, _) = input.split_at(std::mem::size_of::<i32>());
		Ok(Box::new(i32::from_be_bytes(int_bytes.try_into()?)))
	});
	DecodeElement { decoder, consume_size: std::mem::size_of::<i32>() }
}

// Parses 64 bit timestamp value
fn consume_bigendian_timestamp() -> DecodeElement {
	let decoder = Box::new(|input: &[u8]| -> Result<Box<dyn DecodeResult>> {
		let (ts_bytes, _) = input.split_at(std::mem::size_of::<u64>());
		Ok(Box::new(u64::from_be_bytes(ts_bytes.try_into()?)))
	});
	DecodeElement { decoder, consume_size: std::mem::size_of::<u64>() }
}

// Parses blake2b hash and output it as a hex string
fn consume_blake2b_hash() -> DecodeElement {
	let decoder = Box::new(|input: &[u8]| -> Result<Box<dyn DecodeResult>> {
		let (hash_bytes, _) = input.split_at(std::mem::size_of::<H256>());
		let hash: &[u8; 32] = hash_bytes.try_into()?;

		Ok(Box::new(hex::encode(hash)))
	});
	DecodeElement { decoder, consume_size: std::mem::size_of::<H256>() }
}

// Parses known pattern
fn consume_known_string(pat: &str) -> DecodeElement {
	let owned_pat = pat.to_string();
	let pat_len = pat.len();
	let decoder = Box::new(move |input: &[u8]| -> Result<Box<dyn DecodeResult>> {
		let (str_bytes, _) = input.split_at(pat_len);
		let parsed_string = String::from_utf8_lossy(str_bytes).into_owned();
		if parsed_string != owned_pat {
			Err(eyre!("unmatched string: \"{}\", expected: \"{}\"", parsed_string, owned_pat))
		} else {
			Ok(Box::new(parsed_string))
		}
	});
	DecodeElement { decoder, consume_size: pat_len }
}

// Parses an unknown pattern with known length
fn consume_unknown_string(len: usize) -> DecodeElement {
	let decoder = Box::new(move |input: &[u8]| -> Result<Box<dyn DecodeResult>> {
		let (str_bytes, _) = input.split_at(len);
		let parsed_string = String::from_utf8_lossy(str_bytes).into_owned();
		Ok(Box::new(parsed_string))
	});
	DecodeElement { decoder, consume_size: len }
}

fn parse_format_string(fmt_string: &str) -> Result<Vec<DecodeElement>> {
	let mut ret: Vec<DecodeElement> = vec![];
	let mut str_remain = fmt_string;

	while !str_remain.is_empty() {
		let percent_pos = str_remain.find('%');

		match percent_pos {
			Some(pos) => {
				let (plain_string, leftover) = str_remain.split_at(pos);

				if !plain_string.is_empty() {
					ret.push(consume_known_string(plain_string));
				}

				if leftover.len() < 2 {
					// Invalid percent encoding
					return Err(eyre!("invalid percent encoding after {}: {}", plain_string, leftover))
				}

				let percent_char = leftover.get(1..2).unwrap();
				let mut pos = 2;

				let decoder = match percent_char.chars().next().unwrap() {
					'i' => consume_bigendian_i32(),
					't' => consume_bigendian_timestamp(),
					'h' => consume_blake2b_hash(),
					's' => {
						// Parse something like %s2 for a string of 2 characters
						let digits_start = leftover
							.strip_prefix("%s")
							.ok_or_else(|| eyre!("invalid string format: {}", leftover))?;
						let string_size_format =
							digits_start.chars().take_while(|c| c.is_digit(10)).collect::<String>();
						let string_size = string_size_format.parse::<usize>()?;
						if string_size == 0 {
							return Err(eyre!("invalid string format: {}", leftover))
						}
						pos += string_size_format.len();
						consume_unknown_string(string_size)
					},
					_ => return Err(eyre!("invalid percent encoding after {}: {}", plain_string, percent_char)),
				};
				ret.push(decoder);
				str_remain = leftover.get(pos..).unwrap();
			},
			None => {
				str_remain = "";
				ret.push(consume_known_string(str_remain));
			},
		}
	}

	Ok(ret)
}

pub type DecodedOutput = Vec<Vec<Box<dyn DecodeResult>>>;

pub fn decode_keys<D: IntrospectorKvdb>(
	db: &D,
	column: &str,
	decode_fmt: &str,
	lim: &Option<usize>,
) -> Result<DecodedOutput> {
	let percent_pos = decode_fmt.find('%');
	let mut final_result: DecodedOutput = vec![];

	if let Some(pos) = percent_pos {
		let (prefix, _) = decode_fmt.split_at(pos);
		let decoders = parse_format_string(decode_fmt)?;
		let expected_key_len: usize = decoders.iter().map(|elt| elt.consume_size).sum();

		let iter = db.prefixed_iter_values(column, prefix)?;

		for (k, _) in iter {
			if k.len() != expected_key_len {
				return Err(eyre!("invalid key size: {}; expected key size: {}", k.len(), expected_key_len))
			}

			let mut remain = &*k;

			let cur: Vec<_> = decoders
				.iter()
				.map(move |decoder| {
					let (cur, next) = remain.split_at(decoder.consume_size);
					remain = next;
					(decoder.decoder)(cur)
				})
				.try_collect()?;

			final_result.push(cur);

			if let Some(lim) = lim {
				if final_result.len() > *lim {
					final_result.truncate(*lim);
					break
				}
			}
		}
	}

	Ok(final_result)
}
