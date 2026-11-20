# Chain checkpoints

Published checkpoints let a new node reject a long-range forged chain from
genesis. The daemon accepts `--checkpoint HEIGHT:HASH` and persists it to
`<data-dir>/checkpoint.json`; on later starts the file is loaded even without
the flag. The file is bound to the local genesis hash and size-capped; a pin
from another chain or a corrupt file fails startup. A lower `--checkpoint`
does not silently replace a higher persisted pin; delete the file to reset.
When set:

- the main-chain block at `HEIGHT` must have exactly `HASH`; and
- once the local chain has reached `HEIGHT`, a reorg whose fork point is below
  `HEIGHT` is refused. Below a future checkpoint, a reorg is allowed only when
  the fork point is close enough to `HEIGHT` that the pinned branch remains
  recoverable within the journal depth; deeper pre-checkpoint reorgs are
  refused.

The checkpoint is optional for testnets. **Every mainnet node should use the
published checkpoint after launch.** It is not a consensus rule; a node without
it still validates blocks normally, but remains exposed to a from-genesis
forgery.

## Mainnet

Published after 10,000 blocks (about seven days) once at least two seeds agree.

| Height | Block hash | Published | Evidence |
|---|---|---|---|
| — | pending launch | — | — |

### Publish procedure

1. On at least two independent seeds, read the same height:
   `curl -s -H "X-API-Key: $(cat /etc/vtorrent/rpc-api-key)" \
   localhost:22525/api/v1/blockchain/block/height/10000`.
2. Confirm both responses report the same hash.
3. Fill the table above and the release notes.
4. Restart each seed with `--checkpoint 10000:<hash>` and confirm the log line
   `Enforcing operator checkpoint`.
5. Tell operators to start new nodes with the same argument.

## Testnet / regtest

No checkpoint is required. A local checkpoint is useful when rehearsing the
launch procedure; use a height already on the local chain and its exact hash.
