# Regtest Smoke Tests

End-to-end smoke tests that exercise the vTorrent daemon against a local
regtest network. They are shell scripts (no test framework) so they can be run
standalone or wired into CI once GitHub Actions billing is restored.

## Prerequisites

- A built daemon binary: `cargo build -p vtorrent-daemon`
- `curl`
- For `bip158.sh` only: Docker + a Bitcoin Core regtest node

## Running

```bash
# Build the daemon first
cargo build -p vtorrent-daemon

# Two-node P2P connectivity + block propagation
./scripts/smoke/two-node.sh

# VTR-side atomic swap (fund + claim)
./scripts/smoke/vtr-swap.sh

# VTR-side refund (mocktime + refund)
./scripts/smoke/vtr-refund.sh

# Bitcoin BIP-158 UTXO scan (requires a bitcoind regtest container)
./scripts/smoke/bip158.sh
```

Each script starts its own daemon(s) on fixed localhost ports, cleans up on
exit (via a trap), and exits non-zero on failure.

## Bitcoin regtest node (for `bip158.sh`)

```bash
docker run -d --name vtr-btc-regtest -p 18444:18444 \
  ruimarinho/bitcoin-core:latest \
  -regtest -printtoconsole -server=1 \
  -rpcuser=user -rpcpassword=pass -rpcallowip=0.0.0.0/0 -rpcbind=0.0.0.0 \
  -fallbackfee=0.0002 -blockfilterindex=1 -peerblockfilters=1

# Create a wallet and mine some blocks
docker exec vtr-btc-regtest bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass createwallet test
docker exec vtr-btc-regtest bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass \
  -rpcwallet=test generatetoaddress 101 <address>
```

The `-blockfilterindex=1` and `-peerblockfilters=1` flags are **required**: the
former builds the compact-filter index, the latter serves it to peers. Without
them the daemon's BIP-158 scan will fail with `P2P error: early eof`. You can
verify a container has the index enabled with:

```bash
docker exec vtr-btc-regtest bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass \
  getblockfilter 0000000000000000000000000000000000000000000000000000000000000000
# Should return a filter JSON, NOT "Index is not enabled for filtertype basic"
```

If the index is missing, recreate the container:

```bash
docker rm -f vtr-btc-regtest
docker run -d --name vtr-btc-regtest -p 18444:18444 \
  ruimarinho/bitcoin-core:latest \
  -regtest -printtoconsole -server=1 \
  -rpcuser=user -rpcpassword=pass -rpcallowip=0.0.0.0/0 -rpcbind=0.0.0.0 \
  -fallbackfee=0.0002 -blockfilterindex=1 -peerblockfilters=1
docker exec vtr-btc-regtest bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass createwallet test
docker exec vtr-btc-regtest bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass \
  -rpcwallet=test generatetoaddress 101 "$(docker exec vtr-btc-regtest bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass -rpcwallet=test getnewaddress)"
```

## Test key material

The VTR smoke tests use deterministic, regtest-only keys (defined in
`lib.sh`). They are not secret and must never be used on mainnet.
