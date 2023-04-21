# Polkadot Block Time Monitor

Monitors block production time via different RPC nodes. The tool runs in either CLI or Prometheus mode. CLI mode outputs live ASCII charts on the terminal while Prometheus mode exposes an endpoint for scraping the observed block times.

```
cargo run --features=polkadot --bin polkadot-block-time cli \
    --ws=wss://westmint-rpc.polkadot.io:443,wss://wss.moonriver.moonbeam.network:443,wss://statemine-rpc.polkadot.io:443,wss://ws.calamari.systems:443,wss://rpc.polkadot.io:443,wss://kusama-rpc.polkadot.io:443,wss://api.westend.encointer.org:443
```
