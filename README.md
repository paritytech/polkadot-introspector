# Polkadot introspector

**IMPORTANT NOTE: WORK IN PROGRESS!** Some things might not always work as expected.

The Polkadot Introspector is a collection of tools for monitoring and introspection of Polkadot or other substrate-based blockchains.

The tools utilize data sources such as the node's JSON-RPC (over [jsonrpsee](https://github.com/paritytech/jsonrpsee/)) or [substrate-telemetry](https://github.com/paritytech/substrate-telemetry/) to generate output. Depending on the tool used, the data source and output may differ. For examples of how this data can be visualized in Grafana, please see the section about [Grafana dashboards](grafana/README.md).

## Tools available

- [polkadot-parachain-tracer](parachain-tracer/README.md) - Parachain progress monitoring and debugging utility
- [polkadot-block-time](block-time/README.md) - display the current block time in the Substrate-based network
- [polkadot-kvdb](kvdb/README.md) - inspect key-value database used by parachains or the relay chain
- [polkadot-whois](whois/README.md) - tracking of validators using on-chain and substrate telemetry data.

## Building

Chain data is decoded without the runtime metadata, using hand-written decoders in `essentials/src/api/decode.rs`. If a runtime changes the layout of a value we read, the decoder has to be updated.
