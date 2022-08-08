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
//
//! Provides subxt connection, data source, output interfaces and abstractions.
//!
//! Implements two interfaces: event subscription and a subxt wrapper. Both of these
//! build on the simplifying assumption that all errors are hidden away from callers.
//! This trades off control of behavior of errors in favor of simplicity and readability.
//!
//! TODO(ASAP): create issues for all below:
//! TODO: retry logic needs to be improved - exponential backoff, cli options
//! TODO: integration tests for polkadot/parachains.
//! TODO: move prometheus into a module.
//! TODO: decouple the Request/Responses from the subxt definitions.
//! TODO: expose storage via event/api. Build a new event source such that new tools
//! can be built by combining existing ones by listening to storage update events.

pub mod api;
pub mod constants;
pub mod consumer;
pub mod storage;

#[allow(clippy::enum_variant_names)]
mod subxt_wrapper;

pub use self::subxt_wrapper::*;
pub use api::*;
pub use constants::*;
pub use consumer::*;
pub use storage::*;
