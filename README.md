# Polkadot introspector

**IMPORTANT NOTE: WORK IN PROGRESS!** Do not expect this to be working (or supported).

The introspector is a collection of tools for monitoring and introspection of the Polkadot or other substrate based blockchains
via [subxt](https://github.com/paritytech/subxt/).

Depending on the tool, the data source and output might differ.

### Collector tool

In the collector mode, introspector connects to one or multiple (WIP) polkadot validators to observe
runtime events. The primary goal of this mode is to provide a backend for parachain candidates introspection including the following:

* Candidates baking and inclusion time
* Possible disputes and outcomes
* Link to the relay chain
* (WIP) Availability distribution
* (WIP) Approval votes

`cargo run --release --  -vvv collector --url wss://rpc.polkadot.io:443 --listen-addr 127.0.0.1:3030`

The collector provides both websocket API for subscription to the runtime events filtered and the
historical data API to obtain and query the existing state.

### Block time monitor

In this mode, introspector monitors block production time via different RPC nodes. The tool runs in either CLI or Prometheus mode. CLI mode outputs
live ASCII charts on the terminal while Prometheus mode exposes an endpoint for scraping the observed block times.

`cargo run --release -- block-time-monitor --ws=wss://westmint-rpc.polkadot.io:443,wss://wss.moonriver.moonbeam.network:443,wss://statemine-rpc.polkadot.io:443,wss://ws.calamari.systems:443,wss://rpc.polkadot.io:443,wss://kusama-rpc.polkadot.io:443,wss://api.westend.encointer.org:443 cli`
