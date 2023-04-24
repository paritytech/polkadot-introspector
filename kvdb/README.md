## polkadot-kvdb

This tool is designed to extract useful data from the key value database (RocksDB and ParityDB are supported so far).
Subcommands supported:

- **columns** - list available columns
- **usage** - show disk usage for keys and values with the ability to limit scan by specific column and/or a set of key prefixes
- **decode-keys** - decode keys from the database using a format string
- **dump** - dump a live[^1] database to another directory in a set of different formats

`usage` and `keys` subcommands support both human-readable and JSON output formats for automatic checks.

### Usage mode

In this mode, introspector shows disk space usage for keys and values.

```
USAGE:
    polkadot-kvdb --db <DB> usage [OPTIONS]

OPTIONS:
    -c, --column <COLUMN>              Check only specific column(s)
    -h, --help                         Print help information
    -p, --keys-prefix <KEYS_PREFIX>    Limit scan by specific key prefix(es)
```

- `--db <DB>` (required): Specifies the path to the database that the tool will operate on. Replace <DB> with the path to your database.
- `-c`, `--column <COLUMN>` (optional): This option allows you to check only specific column(s) in the database. Replace <COLUMN> with the column number or a comma-separated list of column numbers you want to check.
- `-h`, `--help` (optional): Prints help information about the usage of the CLI tool.
- `-p`, `--keys-prefix <KEYS_PREFIX>` (optional): This option allows you to limit the scan to specific key prefix(es). Replace <KEYS_PREFIX> with the desired key prefix or a comma-separated list of key prefixes you want to scan.

### Decode keys mode

In this mode, polkadot-kvdb allows to decode keys using format strings.

```
USAGE:
    polkadot-kvdb --db <DB> decode-keys [OPTIONS] --column <COLUMN> --fmt <FMT>

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

Format string can currently include plain strings, and one or more percent encoding values, such as `key_%i`. Currently, this module supports the following percent strings:

- `%i` - big endian i32 value
- `%t` - big endian u64 value (timestamp)
- `%h` - blake2b hash represented as hex string
- `%s<d>` - string of length `d` (for example `%s10` represents a string of size 10)

### Dump subcommand

This subcommand is designed to dump the database to another output directory in a set of output formats:

- **RocksDB** - dump database in RocksDB format
- **ParityDB** - dump database in ParityDB format, in this mode all input columns are treated as ordered columns (btree), dump of non-ordered columns is currently limited
- **JSON** - output database as a set of [new-line separated JSON](http://ndjson.org/) files, one for each column

```
USAGE:
    polkadot-kvdb --db <DB> dump [OPTIONS] --output <OUTPUT>

OPTIONS:
    -c, --column <COLUMN>              Check only specific column(s)
    -h, --help                         Print help information
    -o, --output <OUTPUT>              Output directory to dump
        --output-type <OUTPUT_TYPE>    Output database type [default: RocksDB]
    -p, --keys-prefix <KEYS_PREFIX>    Limit scan by specific key prefix(es)
```

[^1]: Live mode is currently supported for RocksDB only

#### Deployment guide

Because `polkadot-kvdb` is dependent upon a Substrate/Polkadot node, it is important to be aware of how to deploy the introspector alongside a node. The subcommand documentation added above should be enough help to get you started.

#### Kubernetes based deployments

For Kubernetes based deployments of the `polkadot-kvdb`, it is important to state that it should be deployed in the same `Pod` as a validator, or collator using the `extraContainers` section in the [public helm chart for Polkadot/Substrate](https://github.com/paritytech/helm-charts/blob/79b8196a9751de50fde21069c9ba4ceebcc858c4/charts/node/values.yaml).

#### Example section

```yaml
extraContainers:
  - name: relaychain-kvdb-introspector
    image: registry/<image-name>:<tag>
    command:
      [
        "polkadot-kvdb",
        "-v",
        "--db",
        "/data/chains/testnet-01/db/full",
        "--db-type",
        "rocksdb",
        "prometheus",
        "--port",
        "9620",
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

Note also that the `prometheus` option is specified. This is useful whenever Prometheus scraping endpoint should be allowed for. The port opened for this purpose is thereafter named `relay-kvdb-prom` which can be used by a Prometheus [`PodMonitor`](https://github.com/prometheus-operator/prometheus-operator/blob/main/Documentation/design.md#podmonitor) in a separate release. It is worthwhile mentioning that the `extraContainers` field is defined [on this line in the node helm chart](https://github.com/paritytech/helm-charts/blob/79b8196a9751de50fde21069c9ba4ceebcc858c4/charts/node/values.yaml#L459).\_
