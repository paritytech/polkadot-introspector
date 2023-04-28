# Polkadot introspector

**IMPORTANT NOTE: WORK IN PROGRESS!** Do not expect this to be working (or supported).

The introspector is a collection of tools for monitoring and introspection of the Polkadot or other substrate based blockchains
via a set of tools (for example, [subxt](https://github.com/paritytech/subxt/)).

Depending on the tool, the data source and output might differ. For examples of how this data can be visualised in Grafana, please see the [Grafana dashboards](grafana/README.md) section.

## Building

It is mandatory to specify which `Runtime` the build will target. Currently `Polkadot Introspector` can only build for a single runtime version by enabling one of the features:

- `polkadot` (supports both Kusama and Polkadot)
- `rococo` (supports Rococo and Versi test networks)

`cargo build --profile=release --features=polkadot`

These features will select which metadata to use for decoding block data.

## Updating or supporting a new `Runtime`

Sometimes the `Runtime` version deployed on a network might be newer and incompatible to the metadata
bundled in the repo. We can use `subxt` CLI to bring the `Polkadot Introspector` metadata up to date.

Example for Versi:
`cargo run --release -p subxt-cli -- metadata --format bytes --url wss://versi-rpc-node-0.parity-versi.parity.io:443 > new_metadata.scale`

Use the output file to replace the older in the `assets` folder then rebuild.

## Tools available

- [polkadot-parachain-tracer](parachain-tracer/README.md) - Parachain progress monitoring and debugging utility
- [polkadot-block-time](block-time/README.md) - display the current block time in the substrate based network
- [polkadot-kvdb](kvdb/README.md) - inspect key-value database used by parachains or the relay chain
- [polkadot-whois](whois/README.md) - tracking of validators using a substrate telemetry data.
