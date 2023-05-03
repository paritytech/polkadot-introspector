# polkadot-metadata-checker

**For internal use only.**

The `Runtime` version deployed on a network might be newer and incompatible with the metadata
bundled in the repository. To check whether the metadata is up-to-date, we run `polkadot-metadata-checker` on our CI/CD pipelines:

```
# For Polkadot Metadata
cargo run --features=polkadot --bin polkadot-metadata-checker -- --ws=wss://rpc.polkadot.io:443

# For Rococo Metadata
cargo run --features=rococo --bin polkadot-metadata-checker -- --ws=wss://rococo-rpc.polkadot.io:443
```

See also: [Updating or supporting a new `Runtime`](../essentials/README.md#updating-or-supporting-a-new-runtime)
