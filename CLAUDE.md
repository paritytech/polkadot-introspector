# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Polkadot Introspector is a collection of monitoring and debugging tools for Polkadot and Substrate-based blockchains. It's structured as a Rust workspace with multiple specialized binaries that leverage RPC endpoints and telemetry data.

## Architecture

The project follows a shared library pattern:

- **essentials/** - Core shared functionality including API clients, blockchain subscriptions, telemetry processing, and metadata handling
- **Individual tools** (block-time, parachain-tracer, kvdb, whois, jaeger) - Each is a separate binary crate depending on essentials
- **priority-channel/** - Internal async channel implementation used across tools

### Key Components

**essentials/src/api/** - Unified API client layer supporting both legacy JSON-RPC and new RPC methods, with dynamic runtime support. Key files: `executor.rs` (request execution), `storage.rs` (storage access), `dynamic.rs` (dynamic runtime metadata).

**essentials/src/collector/** - WebSocket-based data collection from substrate telemetry endpoints.

**essentials/src/chain_*_subscription.rs** - Blockchain event subscription patterns: `chain_head_subscription.rs` (live chain head), `chain_events.rs` (decoded events), `historical_subscription.rs` (block range queries).

**Metadata System** - Tools rely on precompiled Polkadot metadata (`essentials/assets/polkadot_metadata.scale`) for decoding chain data. Auto-updated via CI every 12 hours.

### CLI Pattern

All tools use **clap derive** with a consistent structure:
- Common options: `--ws` (WebSocket endpoint), `--verbose/-v`, `--retry-*` options, `--client` (RPC/Light mode)
- Output modes via subcommands: `cli` (terminal output) or `prometheus` (metrics endpoint via warp)
- parachain-tracer additionally supports `--historical --from <block> --to <block>` for block range analysis

## Development Commands

### Building
```bash
cargo build                              # Build all tools
cargo build -p polkadot-parachain-tracer # Build specific tool
cargo build --release                    # Release build
```

### Testing
Tests require a running Polkadot/Substrate test network via Zombienet (Linux only):
```bash
./scripts/ci/zombienet/zombie.sh setup
ZOMBIE_WS_PORT=9900 ./scripts/ci/zombienet/zombie.sh run ./scripts/ci/zombienet/network.toml
WS_URL=ws://127.0.0.1:9900 cargo test --all-targets --workspace
./scripts/ci/zombienet/zombie.sh shutdown
```

Run a single test:
```bash
WS_URL=ws://127.0.0.1:9900 cargo test -p polkadot-parachain-tracer test_name
```

Tests are inline (`#[cfg(test)]` modules), not in a separate integration test directory. Some crates have dedicated test utility modules (e.g., `parachain-tracer/src/test_utils.rs`).

### Code Quality
```bash
cargo +nightly fmt --all                        # Format (requires nightly)
cargo clippy --all-targets -- -D warnings       # Lint
cargo check --all-targets --workspace           # Type check
```

### Running Tools
```bash
cargo run -p polkadot-parachain-tracer -- --ws wss://rpc.polkadot.io:443 --para-id 2107 cli
cargo run -p polkadot-parachain-tracer -- --ws wss://rpc.polkadot.io:443 --para-id 2107 --historical --from 16080000 --to 16080050 cli
cargo run -p polkadot-block-time -- --ws=wss://rpc.polkadot.io:443,wss://kusama-rpc.polkadot.io:443 cli
cargo run -p polkadot-kvdb -- --db /path/to/rocksdb usage
```

## Code Conventions

- **Rust Edition 2024** with async-first design using Tokio
- **Hard tabs**, 120 character line width (see rustfmt.toml)
- **Error handling** - `thiserror` for custom errors, `color-eyre` for CLI error reporting
- **Async patterns** - `tokio::spawn` for task spawning, `tokio::select!` for multiplexing, bounded channels for backpressure, `tokio::sync::Mutex`/`RwLock` for shared state
- **Imports** - Crate-level granularity, reordered (see rustfmt.toml `imports_granularity = "Crate"`)

## Important Notes

- **Linux requirement** - Testing infrastructure (Zombienet) only works on Linux
- **Network dependency** - Most tools require live RPC endpoints to Polkadot/Substrate chains
- **Metadata updates** - `subxt metadata --format bytes --url wss://rpc.polkadot.io:443 > essentials/assets/polkadot_metadata.scale`
- **Docker** - `scripts/ci/dockerfiles/polkadot-introspector_injected.Dockerfile`
- **Provisional types** - Some types not yet in Polkadot metadata are implemented locally and should be removed once upstream catches up
