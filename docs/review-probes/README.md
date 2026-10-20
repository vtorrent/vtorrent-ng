# Whole-codebase review probes

These are defect demonstrations for source commit
`e604cf041fba5eaa289c04ae911f11669d54e3ad`, not default regression tests.
Run them in a disposable checkout. Failing assertions are the expected result
on that source revision. The compressed legacy-key control should pass.

After the post-review fixes, the BTC boundary, uncompressed-key, empty-torrent,
and soak probes pass. The RPC wire-expansion probe still demonstrates the
serialization expansion; the handler now rejects that response and falls back
to `notfound`, covered by `blocktxn_payload_rejects_oversized_final_message`.
The bootstrap regression now lives permanently in
`vtorrent-node/src/chain/chain_tests.rs`.

The review report is [the whole-codebase review](../2026-10-20_codebase-vtorrent-report.md).
The probes use synthetic keys and data. None requires the running fleet,
Docker, bitcoind, or a network connection.

## Public API probes

From the repository root, install the probe sources as temporary integration
tests in the disposable checkout:

```bash
mkdir -p vtorrent-btc/tests vtorrent-migrate/tests vtorrent-torrent/tests vtorrent-rpc/tests
cp docs/review-probes/btc.rs vtorrent-btc/tests/audit_review_probes.rs
cp docs/review-probes/migrate.rs vtorrent-migrate/tests/audit_review_probes.rs
cp docs/review-probes/torrent.rs vtorrent-torrent/tests/audit_review_probes.rs
cp docs/review-probes/rpc.rs vtorrent-rpc/tests/audit_review_probes.rs
cargo test -p vtorrent-btc --test audit_review_probes -- --nocapture
cargo test -p vtorrent-migrate --test audit_review_probes -- --nocapture
cargo test -p vtorrent-torrent --test audit_review_probes -- --nocapture
cargo test -p vtorrent-rpc --test audit_review_probes -- --nocapture
```

The commands intentionally return nonzero on the reviewed revision. Run each
command independently so the first reproduced defect does not hide the rest.

| Probe | Observed result |
| --- | --- |
| Retained BTC heights 4, 5, 6 | Empty scan result instead of three headers |
| Compressed encrypted legacy key | Correctly decrypted |
| Uncompressed encrypted legacy key | Rejected despite correct ciphertext and key |
| Repeated empty BitTorrent piece frames | All 5,000 admitted |
| Metrics required by alert rules | `vtorrent_best_peer_height` missing |
| Serialized compact-block transaction response | 4,787,037 bytes, exceeding the 4,194,304-byte wire limit |

The wire probe checks serialization and stateless transaction validity. It
does not build and deliver a signed, chain-accepted block over a socket.

## Bootstrap maturity regression

The reviewed-revision probe used the node's synthetic legacy-balance fixture.
Its regression now lives permanently in
`vtorrent-node/src/chain/chain_tests.rs` as
`test_bootstrap_age_exemption_does_not_extend_to_height3`:

```bash
cargo test -p vtorrent-node --lib test_bootstrap_age_exemption_does_not_extend_to_height3 -- --nocapture
```

The reviewed revision accepted the second immature stake at height 3. The
fixed validator requires candidate height 2 for the bootstrap exemption.

## Missing soak-evidence probe

```bash
python3 docs/review-probes/soak_missing_evidence.py
```

This runs the actual observer script with isolated fake `curl` and `docker`
commands. Other evidence is healthy. Both an empty Prometheus result and an
invalid JSON response produce exit 0 from the observer. The probe itself
returns 1 when it finds this false-green behavior. It does not call the
notification wrapper or access real containers.

## CI baseline argument check

```bash
bash scripts/bench-gate.sh --compare-ref ""
```

This returns 2 before running benchmarks. The scheduled build workflow can
produce this empty argument because neither a PR base SHA nor a push `before`
SHA is present in that event.
