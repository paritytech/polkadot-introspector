# Polkadot introspector

**IMPORTANT NOTE: WORK IN PROGRESS!** Do not expect this to be working (or supported).

The introspector is a collection of tools for monitoring and introspection of the Polkadot or other substrate based blockchains
via a set of tools (for example, [subxt](https://github.com/paritytech/subxt/)).

Depending on the tool, the data source and output might differ.

## Tools available

* [Collector tool](#collector-tool) - observe and monitor runtime events via subxt
* [Block time monitor](#block-time-monitor) - display the current block time in the substrate based network
* [KVDB tool](#kvdb-introspection-tool) - inspect key-value database used by parachains or the relay chain

### Collector tool

In the collector mode, introspector connects to one or multiple (WIP) polkadot validators to observe
runtime events. The primary goal of this mode is to provide a backend for parachain candidates introspection including the following:

* Candidates baking and inclusion time
* Possible disputes and outcomes
* Link to the relay chain
* (WIP) Availability distribution
* (WIP) Approval votes

```
cargo run --release -- \
    -vvv collector --url wss://rpc.polkadot.io:443 \
    --listen 127.0.0.1:3030
```

The collector provides both websocket API for subscription to the runtime events filtered and the
historical data API to obtain and query the existing state.

### Block time monitor

In this mode, introspector monitors block production time via different RPC nodes. The tool runs in either CLI or Prometheus mode. CLI mode outputs
live ASCII charts on the terminal while Prometheus mode exposes an endpoint for scraping the observed block times.

```
cargo run --release -- block-time-monitor \
    --ws=wss://westmint-rpc.polkadot.io:443,wss://wss.moonriver.moonbeam.network:443,wss://statemine-rpc.polkadot.io:443,wss://ws.calamari.systems:443,wss://rpc.polkadot.io:443,wss://kusama-rpc.polkadot.io:443,wss://api.westend.encointer.org:443 \
    cli
```

### KVDB introspection tool

This mode is designed to extract useful data from the key value database (RocksDB and ParityDB are supported so far).
Subcommands supported:

* **columns** - list available columns
* **usage** - show disk usage for keys and values with the ability to limit scan by specific column and/or a set of key prefixes
* **keys** - decode keys from the database using a format string

`usage` and `keys` subcommands support both human-readable and JSON output formats for automatic checks.

```
USAGE:
    polkadot-introspector kvdb --db <DB> usage [OPTIONS]

OPTIONS:
    -c, --column <COLUMN>              Check only specific column(s)
    -h, --help                         Print help information
    -p, --keys-prefix <KEYS_PREFIX>    Limit scan by specific key prefix(es)
```

#### Keys format string specification

Format string can currently include plain strings, and one or more percent encoding values, such as `key_%i`. Currently, this module supports the  following percent strings:
 - `%i` - big endian i32 value
 - `%t` - big endian u64 value (timestamp)
 - `%h` - blake2b hash represented as hex string
 - `%s<d>` - string of length `d` (for example `%s10` represents a string of size 10)


