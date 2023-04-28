# polkadot-block-time

RPC based block production time monitoring of multiple chains via inherent timestamps. The tool runs in either CLI or Prometheus mode. CLI mode outputs live ASCII charts on the terminal while Prometheus mode exposes an endpoint for scraping the observed block times.

```
cargo run --features=polkadot --bin polkadot-block-time -- --ws=wss://rpc.polkadot.io:443,wss://kusama-rpc.polkadot.io:443 cli
```
