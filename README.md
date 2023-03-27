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

* [Block time monitor](#block-time-monitor) - display the current block time in the substrate based network
* [KVDB tool](#kvdb-introspection-tool) - inspect key-value database used by parachains or the relay chain
* [Parachain commander](#parachain-commander) - Parachain progress monitoring and debugging utility

### Parachain commander
A parachain progress monitor and debugger which uses `on-chain` data to trace parachain candidates during backing and inclusion.

The tool documentation is available [here](introspector/src/pc/README.md).

This is how it looks when it's running:
![Tracing a parachain on Kusama](img/pc1.png)


### Block time monitor

In this mode, introspector monitors block production time via different RPC nodes. The tool runs in either CLI or Prometheus mode. CLI mode outputs
live ASCII charts on the terminal while Prometheus mode exposes an endpoint for scraping the observed block times.

```
cargo run --release --features=polkadot -- block-time-monitor \
    --ws=wss://westmint-rpc.polkadot.io:443,wss://wss.moonriver.moonbeam.network:443,wss://statemine-rpc.polkadot.io:443,wss://ws.calamari.systems:443,wss://rpc.polkadot.io:443,wss://kusama-rpc.polkadot.io:443,wss://api.westend.encointer.org:443 \
    cli
```

### KVDB introspection tool

This mode is designed to extract useful data from the key value database (RocksDB and ParityDB are supported so far).
Subcommands supported:

* **columns** - list available columns
* **usage** - show disk usage for keys and values with the ability to limit scan by specific column and/or a set of key prefixes
* **decode-keys** - decode keys from the database using a format string
* **dump** - dump a live[^1] database to another directory in a set of different formats

`usage` and `keys` subcommands support both human-readable and JSON output formats for automatic checks.


### Usage mode

In this mode, introspector shows disk space usage for keys and values.

```
USAGE:
    polkadot-introspector kvdb --db <DB> usage [OPTIONS]

OPTIONS:
    -c, --column <COLUMN>              Check only specific column(s)
    -h, --help                         Print help information
    -p, --keys-prefix <KEYS_PREFIX>    Limit scan by specific key prefix(es)
```

### Decode keys mode

In this mode, introspector allows to decode keys using format strings.

```
USAGE:
    polkadot-introspector kvdb --db <DB> decode-keys [OPTIONS] --column <COLUMN> --fmt <FMT>

OPTIONS:
    -c, --column <COLUMN>
            Check only specific column(s)

    -f, --fmt <FMT>
            Decode keys matching the specific format (like `candidate-votes%i%h`, where `%i`
            represents a big-endian integer)

    -h, --help
            Print help information

    -i, --ignore-failures <ignore-failures>
            Allow to ignore decode failures [default: false]

    -l, --limit <LIMIT>
            Limit number of output entries
```

#### Keys format string specification

Format string can currently include plain strings, and one or more percent encoding values, such as `key_%i`. Currently, this module supports the  following percent strings:
- `%i` - big endian i32 value
- `%t` - big endian u64 value (timestamp)
- `%h` - blake2b hash represented as hex string
- `%s<d>` - string of length `d` (for example `%s10` represents a string of size 10)

### Dump subcommand

This subcommand is designed to dump the database to another output directory in a set of output formats:

* **RocksDB** - dump database in RocksDB format
* **ParityDB** - dump database in ParityDB format, in this mode all input columns are treated as ordered columns (btree), dump of non-ordered columns is currently limited
* **JSON** - output database as a set of [new-line separated JSON](http://ndjson.org/) files, one for each column

```
USAGE:
    polkadot-introspector kvdb --db <DB> dump [OPTIONS] --output <OUTPUT>

OPTIONS:
    -c, --column <COLUMN>              Check only specific column(s)
    -h, --help                         Print help information
    -o, --output <OUTPUT>              Output directory to dump
        --output-type <OUTPUT_TYPE>    Output database type [default: RocksDB]
    -p, --keys-prefix <KEYS_PREFIX>    Limit scan by specific key prefix(es)
```

[^1]: Live mode is currently supported for RocksDB only

#### Deployment guide

Because `polkadot-introspector` is dependent upon a Substrate/Polkadot node, it is important to be aware of how to deploy the introspector alongside a node. The subcommand documentation added above should be enough help to get you started.

#### Kubernetes based deployments

For Kubernetes based deployments of the `polkadot-introspector`, it is important to state that it should be deployed in the same `Pod` as a validator, or collator using the `extraContainers` section in the [public helm chart for Polkadot/Substrate](https://github.com/paritytech/helm-charts/blob/79b8196a9751de50fde21069c9ba4ceebcc858c4/charts/node/values.yaml).

#### Example section

```
extraContainers:
  - name: relaychain-kvdb-introspector
    image: registry/<image-name>:<tag>
    command: [
      "polkadot-introspector",
      "-v",
      "kvdb",
      "--db",
      "/data/chains/testnet-01/db/full",
      "--db-type",
      "rocksdb",
      "prometheus",
      "--port",
      "9620"
    ]
    resources:
      limits:
        memory: "1Gi"
    ports:
      - containerPort: 9620
        name: relay-kvdb-prom
    volumeMounts:
      - mountPath: /data
        name: chain-data
```

Here the `RocksDB` path is specified as `/data/chains/testnet-01/db/full`. This path must be reachable from the `Pod`.

Note also that the `prometheus` option is specified. This is useful whenever Prometheus scraping endpoint should be allowed for. The port opened for this purpose is thereafter named `relay-kvdb-prom` which can be used by a Prometheus [`PodMonitor`](https://github.com/prometheus-operator/prometheus-operator/blob/main/Documentation/design.md#podmonitor) in a separate release. It is worthwhile mentioning that the `extraContainers` field is defined [on this line in the node helm chart](https://github.com/paritytech/helm-charts/blob/79b8196a9751de50fde21069c9ba4ceebcc858c4/charts/node/values.yaml#L459)._