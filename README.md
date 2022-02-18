# Polkadot introspector

A tool for monitoring and introspection of the Polkadot or other substrate based blockchains

**IMPORTANT NOTE: WORK IN PROGRESS!** Do not expect this to be working (or supported).

## Proposed architecture
The introspector is a collection of tools that connect to RPC nodes to inspect or monitor the chain state.
Depending on the tool, the data source and output might differ.

### Collector tool

`cargo run --release --  -vvv collector --url wss://rpc.polkadot.io:443`

Introspector uses `subxt` to subscribe for a node (or multiple nodes) events.

Each event comes towards the events dispatcher, where individual handlers can subscribe to events either by pallet or by
a specific combination of pallet + variant.

Each handler is a functor that accepts an event, associated data, such as the current time or a node url, and returns
state of type `<T>`. A handler can work on the level of a block or operate with all blocks in general. For per-block
handlers the output is a sliding window of states, and for general handlers the state is either per-node state or a
global state.

Handlers can chain each other using a simple combinators, such as `and` or `or`. `and` combinator produces dependency
between handlers, whilst `or` means that a state or a raw event might be sent to both handlers simultaneously. For
example, per-block handler can then pass it's current state vector towards a general handler that produces another
state.

One example could be a pipeline where one handler analyses per-block finalise latency and another handle collects a
moving average (or a moving median) of latency for all blocks in general. It should also be possible to extract and
query state for each handler via a websocket/wasm API. The only limitation here is the problem of a consistency, so a
state must not be queried and updated at the same time (where applicable).

### Block time monitor

Monitor block production time via different RPC nodes. The tool runs in either CLI or Prometheus mode. CLI mode outputs
live ASCII charts on the terminal while Prometheus mode exposes an endpoint for scraping the observed block times.

`cargo run --release -- block-time-monitor --ws==wss://westmint-rpc.polkadot.io:443,wss://wss.moonriver.moonbeam.network:443,wss://statemine-rpc.polkadot.io:443,wss://ws.calamari.systems:443,wss://rpc.polkadot.io:443,wss://kusama-rpc.polkadot.io:443,wss://api.westend.encointer.org:443 cli`
