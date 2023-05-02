# polkadot-whois

Tracking of validators using on-chain and substrate telemetry data. The data can be queried by validator's `AccountId` or `(session_index, validator_index)`.

Usage:

```
# by AccountId
cargo run --features=polkadot --bin polkadot-whois -- --ws=wss://rpc.polkadot.io:443  --feed=wss://feed.telemetry.polkadot.io/feed account 1497QNdycmxqMi3VJDxZDhaJh4s9tytr5RFWyrLcNse2xqPD

# by (session_index, validator_index)
cargo run --features=polkadot --bin polkadot-whois -- --ws=wss://rpc.polkadot.io:443  --feed=wss://feed.telemetry.polkadot.io/feed session 1046 12
```
