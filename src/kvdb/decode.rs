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

//! This module contains keys decoding functions according to a user specified
//! format string. Format string can currently include plain strings, and one or
//! more percent encoding values, such as `%i`. Currently, this module supports the
//! following percent strings:
//! - `%i` - big endian `i32` value
//! - `%t` - big endian `u64` value (or a timestamp)
//! - `%h` - blake2 hash represented as hex string
//! - `%s<d>` - string of length `d` (for example `%s10` represents a string of size 10)

use crate::kvdb::IntrospectorKvdb;
use color_eyre::{eyre::eyre, Result};
use erased_serde::{serialize_trait_object, Serialize};
use itertools::Itertools;
use std::fmt::{Debug, Display, Formatter};
use subxt::sp_core::H256;

/// Decode result trait, used to display and format output of the decoder
pub trait DecodeResult: Debug + Serialize + Display {}
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
							digits_start.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
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

fn process_decoders_pipeline(input: &[u8], decoders: &[DecodeElement]) -> Result<Vec<Box<dyn DecodeResult>>> {
	let mut remain = input;

	let result: Vec<_> = decoders
		.iter()
		.map(move |decoder| {
			if remain.len() >= decoder.consume_size {
				let (cur, next) = remain.split_at(decoder.consume_size);
				remain = next;
				(decoder.decoder)(cur)
			} else {
				Err(eyre!("truncated input: {} bytes remain, {} bytes expected", remain.len(), decoder.consume_size))
			}
		})
		.try_collect()?;
	Ok(result)
}

/// Represents a decode result
#[derive(serde::Serialize)]
pub struct KeyDecodeResult {
	/// Decoded fields
	pub fields: Vec<Box<dyn DecodeResult>>,
	/// Size of the value
	pub value_size: usize,
}

impl Display for KeyDecodeResult {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}; value size: {}", &self.fields, self.value_size)?;
		Ok(())
	}
}

/// Options to decode keys
pub struct KeyDecodeOptions<'a> {
	/// Limit number of entries if needed
	pub lim: &'a Option<usize>,
	/// Ignore failures when decoding keys (TODO: maybe make it a mode)
	pub ignore_failures: bool,
	/// Column to use
	pub column: &'a str,
	/// Decode format
	pub decode_fmt: &'a str,
}

#[derive(serde::Serialize)]
pub struct DecodedOutput(Vec<KeyDecodeResult>);

impl Display for DecodedOutput {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		for elt in &self.0 {
			writeln!(f, "{}", elt)?;
		}
		Ok(())
	}
}

pub fn decode_keys<D: IntrospectorKvdb>(db: &D, opts: &KeyDecodeOptions) -> Result<DecodedOutput> {
	let percent_pos = opts.decode_fmt.find('%');
	let mut final_result = DecodedOutput(vec![]);

	if let Some(pos) = percent_pos {
		let (prefix, _) = opts.decode_fmt.split_at(pos);
		let decoders = parse_format_string(opts.decode_fmt)?;
		let expected_key_len: usize = decoders.iter().map(|elt| elt.consume_size).sum();

		let iter = db.prefixed_iter_values(opts.column, prefix)?;

		for (k, v) in iter {
			if k.len() != expected_key_len && !opts.ignore_failures {
				return Err(eyre!("invalid key size: {}; expected key size: {}", k.len(), expected_key_len))
			}

			let cur = process_decoders_pipeline(&k, &decoders)?;

			final_result.0.push(KeyDecodeResult { fields: cur, value_size: v.len() });

			if let Some(lim) = opts.lim {
				if final_result.0.len() > *lim {
					final_result.0.truncate(*lim);
					break
				}
			}
		}
	}

	Ok(final_result)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn decode_with_format_string(decode_fmt: &str, input: &[u8]) -> Result<DecodedOutput> {
		let decoders = parse_format_string(decode_fmt)?;
		let result = process_decoders_pipeline(input, &decoders)?;

		// Check length after parsing to ensure that we can still parse even if the input is wrong
		let expected_key_len: usize = decoders.iter().map(|elt| elt.consume_size).sum();
		if input.len() != expected_key_len {
			Err(eyre!("invalid length: {}, {} expected", input.len(), expected_key_len))
		} else {
			Ok(DecodedOutput(vec![KeyDecodeResult { fields: result, value_size: 0 }]))
		}
	}

	#[test]
	fn test_decode_string() {
		let good_test_cases = vec!["test", "TeSt", "aaaa", "\0\0\0\0"];

		for case in good_test_cases {
			let res = decode_with_format_string("%s4", case.as_bytes()).unwrap();
			assert_eq!(case, res.0[0].fields[0].to_string());
		}

		let bad_test_cases = vec!["testt", "TeS", ""];
		for case in bad_test_cases {
			assert!(decode_with_format_string("%s4", case.as_bytes()).is_err());
		}
	}

	#[test]
	fn test_decode_i32() {
		let good_test_cases: Vec<(Vec<u8>, i32)> = vec![
			(vec![0x0u8, 0x0, 0x0, 0x1], 1_i32),
			(vec![0x0u8, 0x0, 0x0, 0x0], 0_i32),
			(vec![0xffu8, 0xff, 0xff, 0xff], -1),
		];

		for (case, expected) in good_test_cases {
			let res = decode_with_format_string("%i", case.as_slice()).unwrap();
			assert_eq!(expected, res.0[0].fields[0].to_string().parse::<i32>().unwrap());
		}
	}

	#[test]
	fn decode_complex() {
		// Format string + Input + Expected output
		let good_test_cases: Vec<(&str, Vec<u8>, Vec<&str>)> = vec![
			("%s2%i", vec![0x21, 0x21, 0x0u8, 0x0, 0x0, 0x1], vec!["!!", "1"]),
			("!!%i", vec![0x21, 0x21, 0x0u8, 0x0, 0x0, 0x1], vec!["!!", "1"]),
		];

		for (fmt_string, case, expected) in good_test_cases {
			let res = decode_with_format_string(fmt_string, case.as_slice()).unwrap();

			for (idx, elt) in res.0[0].fields.iter().enumerate() {
				assert_eq!(expected[idx], elt.to_string());
			}
		}

		let bad_test_cases: Vec<(&str, Vec<u8>)> = vec![
			// Extra byte
			("%s2%i", vec![0x21, 0x21, 0x21, 0x0u8, 0x0, 0x0, 0x1]),
			// Unmatching static string
			("!?%i", vec![0x21, 0x21, 0x0u8, 0x0, 0x0, 0x1]),
			// Truncated input
			("!!%i", vec![0x21, 0x21, 0x0u8, 0x0, 0x0]),
		];

		for (fmt_string, case) in bad_test_cases {
			let res = decode_with_format_string(fmt_string, case.as_slice());
			assert!(res.is_err());
		}
	}
}
