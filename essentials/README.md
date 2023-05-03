# polkadot-introspector-essentials

This repository contains essential components for the Polkadot Introspector toolset, including pre-built runtime modules for Polkadot- and Rococo-like Substrate-based blockchains.

## Updating or supporting a new `Runtime`

The `Runtime` version deployed on a network might be newer and incompatible with the metadata bundled in the repository. To check whether the metadata is up-to-date, we run [polkadot-metadata-checker](../metadata-checker/README.md) on our CI/CD pipelines. In case it fails, to bring the new metadata, we use [subxt-cli](https://github.com/paritytech/subxt/#downloading-metadata-from-a-substrate-node).

Here's an example of how to update the metadata:

```
# Update metadata for Rococo
subxt metadata --format bytes --url wss://rococo-rpc.polkadot.io:443 > assets/rococo_metadata.scale

# Update metadata for Polkadot
subxt metadata --format bytes --url wss://rpc.polkadot.io:443 > assets/polkadot_metadata.scale
```

After replacing metadata files in the assets' folder with new ones, we need to rebuild the tools.
