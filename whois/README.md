# polkadot-whois

A tool for converting and querying information about validators using interchangeable unique identifiers, information about a validator can be queried using the following indentifiers:
- Validator index in `paraSessionInfo` pallet.
- The account key.
- The authority discovery id, provided in the hex format, it is the same format as what is displayed in `session.next_keys`.
- The peer id, the network peer id used for connecting to other nodes.

It prints a list of information for each queried validator, G.g:
```
validator_index=<INDEX<>>, account=<ACCOUNT>, peer_id=<PEER_ID>, authorithy_id_discover=0x<AUTHORITY_DISCOVERY>, addresses=<KNOWN_ADDRESSES>, version="POLKADOT_NODE_VERSION"
```

It uses the subp2p-explorer for querying the DHT information about each validator in the network.

## Usage:

### Query by validator index
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://rpc.polkadot.io:443 \
 --bootnodes /dns/polkadot-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWSz8r2WyCdsfWHgPyvD8GKQdJ1UAiRmrcrs8sQB3fe2KU \
 --session-index 9442 \
 by-validator-index 295 293
```
### Query by peer id

```
cargo run  --bin polkadot-whois -- \
 --ws=wss://rpc.polkadot.io:443 \
 --bootnodes /dns/polkadot-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWSz8r2WyCdsfWHgPyvD8GKQdJ1UAiRmrcrs8sQB3fe2KU \
 --session-index 9442 \
 by-peer-id 12D3KooWEtD4vrMGsaAmETPx9VXuAu3UyFc7hL92x7ky3TJZwnT7 \
 12D3KooWJJC4ACkC6fvsDuAvcgiebPMVNjMsfL3LtgmitAKhC39N
 
```

### Query by authority discovery key

```
cargo run  --bin polkadot-whois -- \
 --ws=wss://rpc.polkadot.io:443 \
 --bootnodes /dns/polkadot-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWSz8r2WyCdsfWHgPyvD8GKQdJ1UAiRmrcrs8sQB3fe2KU \
 --session-index 9442 \
 by-authority-discovery 0x1cbbd313b592c053da0dc85fe0ae3c010d7bfc3c858a303418a1707846b6507d \
 0xf2fe41ba85a8b16db8642126fd7d4bd3f9cf46c45e9852d528867f3d19474972
```

### Query by account id
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://rpc.polkadot.io:443 \
 --bootnodes /dns/polkadot-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWSz8r2WyCdsfWHgPyvD8GKQdJ1UAiRmrcrs8sQB3fe2KU \
 --session-index 9442 \
 by-account 5D8DuA8a3obyN6ADUTJPUvx5yj8nnKmE1PqsKBQYCRoiqEak
```
### Dump all
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://rpc.polkadot.io:443 \
 --bootnodes /dns/polkadot-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWSz8r2WyCdsfWHgPyvD8GKQdJ1UAiRmrcrs8sQB3fe2KU \
 --session-index 9442 \
 dump-all
```
### Usage examples by network.

## Polkadot
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://rpc.polkadot.io:443 \
 --bootnodes /dns/polkadot-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWSz8r2WyCdsfWHgPyvD8GKQdJ1UAiRmrcrs8sQB3fe2KU \
 --session-index 9442 \
 by-validator-index 295 293
```

## Kusama
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://kusama-rpc.polkadot.io:443 \
 --bootnodes /dns/kusama-bootnode-0.polkadot.io/tcp/30333/p2p/12D3Koo
WSueCPH3puP2PcvqPJdNaDNF3jMZjtJtDiSy35pWrbt5h \
 --address-format kusama --timeout 900  --chain kusama --session-index 42088 \
 by-validator-index 295 293
```

## Westend
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://westend-rpc.polkadot.io:443 \
 --bootnodes /dns/westend-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWKer94o1REDPtAhjtYR4SdLehnSrN8PEhBnZm5NBoCrMC \
 --session-index 42088 \
 by-validator-index 5 2
```
## Rococo
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://rococo-rpc.polkadot.io:443 \
 --bootnodes /dns/rococo-bootnode-0.polkadot.io/tcp/30333/p2p/12D3KooWGikJMBmRiG5ofCqn8aijCijgfmZR5H9f53yUF3srm6Nm \
 --session-index 42088 \
 by-validator-index 295 293
```
## Paseo
```
cargo run  --bin polkadot-whois -- \
 --ws=wss://paseo-rpc.dwellir.com:443 \
 --bootnodes /dns/paseo.bootnode.amforc.com/tcp/29999/wss/p2p/12D3KooWSdf63rZjtGdeWXpQwQwPh8K8c22upcB3B1VmqW8rxrjw \
 --session-index 42088 \
 by-validator-index 11 12

```
