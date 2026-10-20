# Multi-node BTC SPV agreement and reorg test

`independent_daemons_agree_on_btc_spv_tip_and_reorg` runs two independent
vTorrent daemons against a real Bitcoin Core regtest node. It covers the SPV
observation layer that the atomic-swap BTC leg relies on. It does not run a
full HTLC swap.

## Scenario

The test requires the `vtr-btc-regtest` container with block filters enabled
and a funded `multinode` wallet. It derives two BTC seeds from the current
time, so each run funds fresh BIP84 receiving addresses and repeated runs do
not accumulate balances.

The test funds both addresses with 1 BTC, mines one block, and starts both
daemons with `--btc-regtest`, their seed, and `--btc-peer 127.0.0.1:18448`.
Each daemon must sync the real header chain, scan BIP-158 filters, and report:

- `synced: true`
- the same `best_height` as bitcoind
- `balance_satoshis: 100000000`
- its own derived receiving address

Both daemons then restart. The BTC wallet rescans from height 0 because its
scan checkpoint is in memory. Both must rediscover the same tip and balance.

The test invalidates the block that confirmed the funding transactions and
mines two replacement blocks. Bitcoin Core returns the funding transactions
to its mempool and includes them in the replacement chain. Both daemons
restart again and must agree on the higher tip and the re-included 1 BTC
balance.

## Recorded result

The recorded run passed:

| Observation | Result |
| --- | --- |
| Funded BTC tip, recorded run | 245 |
| Reorged BTC tip, recorded run | 246 |
| Daemons agreeing on each tip | 2 |
| Balance per daemon after each restart | 100,000,000 sats |
| Test wall time | about 1.4 seconds |

The recorded run used release mode and the existing regtest container. The
tip numbers depend on the shared container's current height; the assertions
compare each daemon with bitcoind, not with fixed heights. The daemons synced
the full header chain and scanned the corresponding filters in about a second.

## Reproduce

The test is ignored by default because it needs Docker and a running regtest
bitcoind with `-blockfilterindex=1` and `-peerblockfilters=1`.

```bash
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery independent_daemons_agree_on_btc_spv_tip_and_reorg \
    -- --ignored --nocapture
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery multi_node_htlc_swap_settles_across_real_btc_spv \
    -- --ignored --nocapture
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery btc_spv_drops_a_reorged_out_funding_output \
    -- --ignored --nocapture
```

The container must expose P2P on `127.0.0.1:18448` and accept
`bitcoin-cli -regtest -rpcuser=vtr -rpcpassword=vtrtest`. The test creates or
loads the `multinode` wallet and mines 101 blocks if its balance is below
3 BTC. It changes the shared regtest chain tip and wallet state.

## Full HTLC swap

`multi_node_htlc_swap_settles_across_real_btc_spv` runs the complete swap
against the same real regtest bitcoind. It uses the deterministic regtest
maker and taker keys from the smoke tests.

The test funds the maker's BTC wallet, starts the maker daemon with BTC SPV,
faucets 200 VTR, imports and unlocks the maker wallet, and places a 100 VTR
for 100,000 sat order. Matching funds the VTR HTLC into the mempool.

The test-only `POST /api/v1/blockchain/mine` endpoint builds a regtest block
from the current valid mempool transactions plus a coinbase mint. It is
compiled only with `test-util`, requires regtest, and is not part of a
production build. Six calls confirm the VTR HTLC.

The maker then funds the BTC HTLC, mines six BTC blocks, and claims BTC. The
claim handler performs a fresh isolated SPV scan of the exact P2WSH contract,
requiring six confirmations. After six more BTC blocks and a reconciliation,
the BTC observation must be `claimed` with the exact funding and claim txids.

The taker claims VTR with the preimage and taker WIF. Six VTR blocks confirm
the claim. The final status must report `claimed` for the VTR leg with the
exact funding and claim txids.

Recorded result (txids vary per run because the test uses fresh keys):

```text
VTR funding 8c9512158202f30cae467d91f21bdf41d45e10e0742c37072c916c0ffacf4048
BTC funding 0c094b57f53bb57db552870b9802034fe716344f2c9210b1c15e80e7b9df942f
BTC claim   f715794e549ef6e90dd85307b61403ec1554f6afed0785a2585aa104be65526c
VTR claim   e209d2a8add24d0d25e176f780b7095c2eedf053f013e0030fe293235db672c4
```

The test waits for the bitcoind compact-filter index to reach the new tip
before each SPV scan. Without that wait, the scan can time out while the
index catches up after a burst of mined blocks. The production `btc-claim`
and `btc-reconcile` handlers surface a scan timeout as an error; the user can
retry the claim, and the reconciliation loop retries on its next interval.
The test's reconciliation helper also retries transient peer EOFs.

## Reorged-out funding

`btc_spv_drops_a_reorged_out_funding_output` funds a daemon address through a
test-controlled P2WPKH key, confirms the funding, then invalidates its block
and replaces the funding transaction with a conflicting spend of the same
input. The replacement pays a burn address, so the daemon's output disappears.

The restarted daemon must report the new tip and a zero balance. The original
funding transaction must have zero confirmations.

Recorded result (txids vary per run):

```text
Reorged-out BTC funding 1f50b222f566d5766f52fb136f10024be7ec5b46c043a6813e6e5db8f42b7b15
was replaced by 8ab28e13e97bba085ec2260f6fbf01a3640cd359cde37559f244bd9364173f1e;
the restarted daemon reports a zero balance
```

## Limits

The first test proves that two independent daemons agree on real SPV headers,
discover the same funded outputs, persist that view across restarts, and
follow a reorg that re-includes the funding transactions. The third test
proves that a reorged-out funding output disappears after a conflicting spend
and restart.

The full swap test uses one VTR daemon and one real bitcoind. It exercises the
real BTC SPV scan, HTLC funding, maker claim, and taker claim, but it does not
run the maker and taker on separate VTR nodes. The existing VTR refund and
reorg tests cover multi-daemon VTR behavior.

The tests do not cover eclipse attacks, mempool conflicts, or a reorg after a
BTC claim has been verified. Those remain in the external review scope.
