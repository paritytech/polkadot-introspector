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

use log::debug;
use polkadot_introspector_essentials::types::SubxtHrmpChannel;
use std::collections::BTreeMap;

#[derive(Default)]
/// A structure that tracks messages (UMP, HRMP, DMP etc)
pub struct MessageQueuesTracker {
	/// Known inbound HRMP channels, indexed by source parachain id
	pub inbound_hrmp_channels: BTreeMap<u32, SubxtHrmpChannel>,
	/// Known outbound HRMP channels, indexed by source parachain id
	pub outbound_hrmp_channels: BTreeMap<u32, SubxtHrmpChannel>,
}

impl MessageQueuesTracker {
	/// Update the content of HRMP channels
	pub fn set_hrmp_channels(
		&mut self,
		inbound_channels: BTreeMap<u32, SubxtHrmpChannel>,
		outbound_channels: BTreeMap<u32, SubxtHrmpChannel>,
	) {
		debug!("hrmp channels configured: {:?} in, {:?} out", &inbound_channels, &outbound_channels);
		self.inbound_hrmp_channels = inbound_channels;
		self.outbound_hrmp_channels = outbound_channels;
	}

	/// Returns if there are HRMP messages in any direction
	pub fn has_hrmp_messages(&self) -> bool {
		self.inbound_hrmp_channels.values().any(|channel| channel.total_size > 0) ||
			self.outbound_hrmp_channels.values().any(|channel| channel.total_size > 0)
	}

	/// Returns active inbound channels
	pub fn active_inbound_channels(&self) -> Vec<(u32, SubxtHrmpChannel)> {
		self.inbound_hrmp_channels
			.iter()
			.filter(|(_, queue)| queue.total_size > 0)
			.map(|(source_id, queue)| (*source_id, queue.clone()))
			.collect::<Vec<_>>()
	}

	/// Returns active outbound channels
	pub fn active_outbound_channels(&self) -> Vec<(u32, SubxtHrmpChannel)> {
		self.outbound_hrmp_channels
			.iter()
			.filter(|(_, queue)| queue.total_size > 0)
			.map(|(dest_id, queue)| (*dest_id, queue.clone()))
			.collect::<Vec<_>>()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_utils::create_hrmp_channels;

	#[test]
	fn test_set_hrmp_channels() {
		let mut tracker = MessageQueuesTracker::default();
		assert!(!tracker.has_hrmp_messages());

		// if inbound has size
		tracker.set_hrmp_channels(create_hrmp_channels(), Default::default());
		assert!(tracker.has_hrmp_messages());

		// if outbound has size
		tracker.set_hrmp_channels(Default::default(), create_hrmp_channels());
		assert!(tracker.has_hrmp_messages());
	}

	#[test]
	fn test_active_inbound_channels() {
		let mut tracker = MessageQueuesTracker::default();
		assert!(tracker.active_inbound_channels().is_empty());

		let channels = create_hrmp_channels();
		assert!(channels.len() > 1);
		tracker.set_hrmp_channels(channels, Default::default());
		assert_eq!(tracker.active_inbound_channels().iter().map(|v| v.0).collect::<Vec<u32>>(), vec![100])
	}

	#[test]
	fn test_active_outbound_channels() {
		let mut tracker = MessageQueuesTracker::default();
		assert!(tracker.active_outbound_channels().is_empty());

		let channels = create_hrmp_channels();
		assert!(channels.len() > 1);
		tracker.set_hrmp_channels(Default::default(), channels);
		assert_eq!(tracker.active_outbound_channels().iter().map(|v| v.0).collect::<Vec<u32>>(), vec![100])
	}
}
