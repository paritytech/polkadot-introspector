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

//! Conversions from the metadata-generated types to our own, the only place besides the API client
//! that names the generated types.

use crate::{
	metadata::polkadot::{
		runtime_types::{polkadot_runtime_parachains::hrmp::HrmpChannel, sp_consensus_babe, sp_consensus_slots},
		session::storage::types::queued_keys::QueuedKeys as GeneratedQueuedKeys,
	},
	types::{AuthorityDiscoveryId, BabeAuthorityId, QueuedKeys, SessionKeys, Slot, SubxtHrmpChannel},
};

impl From<&HrmpChannel> for SubxtHrmpChannel {
	fn from(channel: &HrmpChannel) -> Self {
		SubxtHrmpChannel {
			max_capacity: channel.max_capacity,
			max_total_size: channel.max_total_size,
			max_message_size: channel.max_message_size,
			msg_count: channel.msg_count,
			total_size: channel.total_size,
			mqc_head: channel.mqc_head,
			sender_deposit: channel.sender_deposit,
			recipient_deposit: channel.recipient_deposit,
		}
	}
}

impl From<HrmpChannel> for SubxtHrmpChannel {
	fn from(channel: HrmpChannel) -> Self {
		Self::from(&channel)
	}
}

impl From<sp_consensus_slots::Slot> for Slot {
	fn from(slot: sp_consensus_slots::Slot) -> Self {
		Slot(slot.0)
	}
}

impl From<&sp_consensus_babe::app::Public> for BabeAuthorityId {
	fn from(public: &sp_consensus_babe::app::Public) -> Self {
		BabeAuthorityId(public.0)
	}
}

pub fn queued_keys(generated: &GeneratedQueuedKeys) -> QueuedKeys {
	generated
		.iter()
		.map(|(account, keys)| {
			(account.clone(), SessionKeys { authority_discovery: AuthorityDiscoveryId(keys.authority_discovery.0) })
		})
		.collect()
}
