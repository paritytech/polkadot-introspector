use serde_json::value::RawValue;
use subxt::utils::H256;

type BlockHash = H256;
type BlockNumber = u64;
type Timestamp = u64;
type FeedNodeId = usize;

#[derive(Debug, PartialEq)]
pub enum TelemetryFeed {
	Version(usize),
	BestBlock { block_number: BlockNumber, timestamp: Timestamp, avg_block_time: Option<u64> },
	BestFinalized { block_number: BlockNumber, block_hash: BlockHash },
	// AddedNode
	RemovedNode { node_id: FeedNodeId },
	LocatedNode { node_id: FeedNodeId, lat: f32, long: f32, city: String },
	// ImportedBlock
	// FinalizedBlock
	// NodeStatsUpdate
	// Hardware
	// TimeSync
	// AddedChain
	// RemovedChain
	// SubscribedTo
	// UnsubscribedFrom
	// Pong
	// StaleNode
	// NodeIOUpdate
	// ChainStatsUpdate
	UnknownValue { action: u8, value: String },
}

impl TelemetryFeed {
	/// Decodes a slice of bytes into a vector of feed messages.
	/// Telemetry sends encoded messages in an array format like [0,32,1,[14783932,1679657352067,5998]]
	/// where odd values represent action codes and even values represent their payloads.
	pub fn from_bytes(bytes: &[u8]) -> color_eyre::Result<Vec<TelemetryFeed>> {
		let v: Vec<&RawValue> = serde_json::from_slice(bytes)?;

		let mut feed_messages = vec![];
		for raw in v.chunks_exact(2) {
			let action: u8 = serde_json::from_str(raw[0].get())?;
			let msg = TelemetryFeed::decode(action, raw[1])?;

			feed_messages.push(msg);
		}

		Ok(feed_messages)
	}

	// Deserializes the feed message to a value based on the "action" key
	fn decode(action: u8, raw_payload: &RawValue) -> color_eyre::Result<TelemetryFeed> {
		let feed_message = match action {
			// Version:
			0 => {
				let version = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::Version(version)
			},
			// BestBlock
			1 => {
				let (block_number, timestamp, avg_block_time) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::BestBlock { block_number, timestamp, avg_block_time }
			},
			// BestFinalized
			2 => {
				let (block_number, block_hash) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::BestFinalized { block_number, block_hash }
			},
			// TODO: Add the following messages
			//  3: AddedNode
			// RemovedNode
			4 => {
				let node_id = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::RemovedNode { node_id }
			},
			// LocatedNode
			5 => {
				let (node_id, lat, long, city) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::LocatedNode { node_id, lat, long, city }
			},
			//  6: ImportedBlock
			//  7: FinalizedBlock
			//  8: NodeStatsUpdate
			//  9: Hardware
			// 10: TimeSync
			// 11: AddedChain
			// 12: RemovedChain
			// 13: SubscribedTo
			// 14: UnsubscribedFrom
			// 15: Pong
			// 20: StaleNode
			// 21: NodeIOUpdate
			// 22: ChainStatsUpdate
			_ => TelemetryFeed::UnknownValue { action, value: raw_payload.to_string() },
		};

		Ok(feed_message)
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn decode_version_best_block_best_finalized() {
		let msg = r#"[0,32,1,[14783932,1679657352067,5998],2,[14783934,"0x0000000000000000000000000000000000000000000000000000000000000000"]]"#;

		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::Version(32),
				TelemetryFeed::BestBlock {
					block_number: 14783932,
					timestamp: 1679657352067,
					avg_block_time: Some(5998)
				},
				TelemetryFeed::BestFinalized { block_number: 14783934, block_hash: BlockHash::zero() }
			]
		);
	}

	#[test]
	fn decode_removed_node_located_node() {
		let msg = r#"[4,42,5[1560,35.6893,139.6899,"Tokyo"]]"#;

		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::RemovedNode { node_id: 42 },
				TelemetryFeed::LocatedNode { node_id: 1560, lat: 35.6893, long: 139.6899, city: "Tokyo".to_owned() }
			]
		);
	}

	#[test]
	fn decode_unknown() {
		let msg = r#"[0,32,42,["0x0000000000000000000000000000000000000000000000000000000000000000", 1]]"#;

		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::Version(32),
				TelemetryFeed::UnknownValue {
					action: 42,
					value: "[\"0x0000000000000000000000000000000000000000000000000000000000000000\", 1]".to_owned()
				}
			]
		);
	}
}
