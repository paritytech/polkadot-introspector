## Parachain Commander (TM)
### What problem does it solve ?
Provides an automated way to digest the `on-chain` information to reason about why a candidate was not backed or was not included in time.
There are two modes of running Parachain Commander: `CLI` and `Prometheus`. In both modes, it can trace only a single parachain. The `CLI` mode is purposed for development use, while `Prometheus` is for monitoring.

### Modes
The CLI mode is designed for debugging parachain's performance issues and to determine what parts of the pipeline are slow.
In general, performance issues always lead to less than optimal block times (> 12s) while this tool is able to recognize the following conditions:
- slow backing - no candidate is backed even if the availability core is free
- slow availability - less than 2/3 + 1 para validators did not receive the erasure coded chunks of a previously backed candidate
- slow bitfield propagation - the relay block author did not receive(via gossip) 2/3 + 1 the signed bitfields from the parachain validators

It's important to note that the tool cannot further drill down into the details and identify the actual root cause of the slowness. This is something that still needs to be done manually via logs/metrics.

**The Prometheus mode is currently not implemented.**

### What's on the roadmap ?
- WIP: backing group information for each backed candidate (validator stash addresses)
- (soon) parachain block times measured in relay chain blocks
- (soon) dispute resolution tracking
- parachain XCM throughput
- parachain code size
- parachain PoV size
- (some time in the future) collator protocol introspection via gossip traffic analysis

### How do I use it ?
Assuming you have cloned the repo, and built the binary, all you need for it to work is a relay chain RPC endpoint.

Example: `polkadot-introspector parachain-commander --ws=wss://kusama-try-runtime-node.parity-chains.parity.io:443 --para-id=2107`

```
USAGE:
    polkadot-introspector parachain-commander [OPTIONS] --para-id <PARA_ID>

OPTIONS:
    -h, --help                 Print help information
        --para-id <PARA_ID>    Parachain id
        --ws <ws>              Websocket url of a relay chain node [default:
                               wss://rpc.polkadot.io:443]
```

