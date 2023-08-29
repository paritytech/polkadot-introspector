# polkadot-introspector-essentials

This repository contains essential components for the Polkadot Introspector toolset, including pre-built runtime modules for Polkadot-like Substrate-based blockchains.

## Updating a `Runtime`

The deployed `Runtime` version on a network might be newer and incompatible with the metadata included in the repository. To keeo the metadata up-to-date, we employ automated metadata updates [within our CI/CD workflow](./../.github/workflows/update_metadata.yml).

For manually updating the metadata, we utilize [subxt-cli](https://github.com/paritytech/subxt/#downloading-metadata-from-a-substrate-node).

```
# Update metadata for Polkadot
subxt metadata --format bytes --url wss://rpc.polkadot.io:443 > assets/polkadot_metadata.scale
```

After replacing the existing metadata file in the assets folder with the updated one, we need to rebuild the tools.
