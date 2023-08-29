# Polkadot introspector

**IMPORTANT NOTE: WORK IN PROGRESS!** Some things might not always work as expected.

The Polkadot Introspector is a collection of tools for monitoring and introspection of Polkadot or other substrate-based blockchains.

The tools utilize data sources such as [subxt](https://github.com/paritytech/subxt/) or [substrate-telemetry](https://github.com/paritytech/substrate-telemetry/) to generate output. Depending on the tool used, the data source and output may differ. For examples of how this data can be visualized in Grafana, please see the section about [Grafana dashboards](grafana/README.md).

## Tools available

- [polkadot-parachain-tracer](parachain-tracer/README.md) - Parachain progress monitoring and debugging utility
- [polkadot-block-time](block-time/README.md) - display the current block time in the Substrate-based network
- [polkadot-kvdb](kvdb/README.md) - inspect key-value database used by parachains or the relay chain
- [polkadot-whois](whois/README.md) - tracking of validators using on-chain and substrate telemetry data.

## Building

We utilize the latest polkadot metadata to decode block data. It is possible that we might lack some types, which are already present in test networks but not yet in polkadot. In such instances, we implement our own provisional types, which should be removed once they are included in the polkadot metadata.

See also: [Updating a `Runtime`](essentials/README.md#updating-a-runtime)
