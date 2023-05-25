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

It is mandatory to specify which `Runtime` the build will target. Currently, the tools can only build for a single runtime version by enabling one of the following features:

- `rococo` - for Rococo and Versi test networks
- `kusama` - for Kusama production networks
- `polkadot` - for Polkadot production networks

These features will select which metadata to use for decoding block data. To enable a specific feature, use the following command:

```
cargo build --release --no-default-features --features=polkadot
```

See also: [Updating or supporting a new `Runtime`](essentials/README.md#updating-or-supporting-a-new-runtime)
