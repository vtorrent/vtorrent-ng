#!/usr/bin/env bash
# Start an isolated three-node release-candidate fleet with BTC SPV enabled.
#
# Every allocation-related setting used by the soak fleet is applied
# explicitly: a BTC-enabled fleet launched without them peaked at 279.7 MiB
# and broke the 256 MiB budget (docs/soak-log.md, 2026-11-01).
#
# Usage:
#   IMAGE=vtorrent/node:candidate-abc1234 scripts/start-release-fleet.sh
#
# Environment overrides:
#   IMAGE        required; local image tag or digest
#   NAME         fleet name; defaults to release-<UTC timestamp>
#   EVIDENCE     evidence directory; defaults to /tmp/opencode/<name>
#   SECRET_DIR   host directory for bind-mounted secrets; must be visible to
#                the Docker daemon (snap Docker cannot read /tmp). Defaults to
#                <repo>/target/<name>-secrets
#   BTC_PEER     initial Bitcoin peer; defaults to 89.58.60.208:8333
#   RPC_BASE     first host RPC port; defaults to 23950
#   P2P_BASE     first host P2P port; unused unless YOU publish it
#   WAIT_SECS    startup wait per node; defaults to 180
#
# The fleet is not scraped by the production Prometheus and the containers use
# container-local data directories, so `docker rm -f` discards their chains.

set -euo pipefail

: "${IMAGE:?set IMAGE to a local vtorrent/node tag or digest}"

STAMP="${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
NAME="${NAME:-release-$STAMP}"
EVIDENCE="${EVIDENCE:-/tmp/opencode/$NAME}"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SECRET_DIR="${SECRET_DIR:-$REPO_ROOT/target/$NAME-secrets}"
BTC_PEER="${BTC_PEER:-89.58.60.208:8333}"
RPC_BASE="${RPC_BASE:-23950}"
WAIT_SECS="${WAIT_SECS:-180}"

# Staking needs regtest faucet funding, so it is opt-out for non-staking
# probes. The default keys are the documented regtest fixtures already used by
# the test suite; override them for anything other than a throwaway fleet.
START_STAKING="${START_STAKING:-1}"
STAKER_PASSPHRASE="${STAKER_PASSPHRASE:-regtest-only}"
STAKER_WIF_2="${STAKER_WIF_2:-WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS}"
STAKER_WIF_3="${STAKER_WIF_3:-WHorCxM7fSQDwTd6W19pdTjSH2nAHrgwEULonwbkDkDw7TdSxTVU}"
FAUCET_AMOUNT="${FAUCET_AMOUNT:-20000000000}"

DATA_DIR=/tmp/vtr-release-fleet

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "error: image not found locally: $IMAGE" >&2
    exit 1
fi

if docker ps -a --format '{{.Names}}' | grep -qx "${NAME}-1"; then
    echo "error: containers named ${NAME}-N already exist; remove them first" >&2
    exit 1
fi

umask 077
mkdir -p "$EVIDENCE" "$SECRET_DIR"
chmod 700 "$EVIDENCE" "$SECRET_DIR"

API_KEY=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf '%s\n' "$API_KEY" > "$SECRET_DIR/api-key"
for i in 1 2 3; do
    head -c 64 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$SECRET_DIR/btc-seed-$i"
done

docker network create "$NAME" >/dev/null

ALLOCATOR=(
    MALLOC_ARENA_MAX=2
    MALLOC_MMAP_THRESHOLD_=131072
    MALLOC_TRIM_THRESHOLD_=131072
)

for i in 1 2 3; do
    args=(
        run -d --name "${NAME}-${i}" --network "$NAME"
        --network-alias "candidate${i}"
        -p "127.0.0.1:$((RPC_BASE + i)):22525"
        --mount "type=bind,src=$SECRET_DIR/api-key,dst=/run/vtr-api-key,readonly"
        --mount "type=bind,src=$SECRET_DIR/btc-seed-${i},dst=/run/vtr-btc-seed,readonly"
    )
    for setting in "${ALLOCATOR[@]}"; do
        args+=(-e "$setting")
    done
    args+=(
        "$IMAGE"
        --regtest --testnet --regtest-fast-stake --isolated --no-dht
        --listen 0.0.0.0:22526
        --rpc-addr 0.0.0.0:22525
        --data-dir "$DATA_DIR"
        --rpc-api-key-file /run/vtr-api-key
        --btc-peer "$BTC_PEER"
        --btc-seed-file /run/vtr-btc-seed
        --btc-header-retention 100000
    )
    if [ "$i" -gt 1 ]; then
        args+=(--seed "candidate1:22526")
    fi
    docker "${args[@]}" >/dev/null
done

for i in 1 2 3; do
    port=$((RPC_BASE + i))
    ready=0
    for _ in $(seq 1 "$WAIT_SECS"); do
        if curl -sf --max-time 3 -H "X-Api-Key: $API_KEY" \
            "http://127.0.0.1:${port}/api/v1/info" >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 1
    done
    if [ "$ready" -ne 1 ]; then
        echo "error: node $i did not answer on port $port" >&2
        exit 1
    fi
done

cat > "$EVIDENCE/deployment.txt" <<EOF
name=$NAME
image=$IMAGE
image_id=$(docker image inspect "$IMAGE" --format '{{.Id}}')
commit=$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)
created_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
btc_peer=$BTC_PEER
rpc_ports=$(seq -s, $((RPC_BASE + 1)) $((RPC_BASE + 3)))
allocator=${ALLOCATOR[*]}
secret_dir=$SECRET_DIR
EOF

# A scrape config that shows the fleet exactly as the production rules expect.
cat > "$EVIDENCE/prometheus.yml" <<EOF
global:
  scrape_interval: 30s
  scrape_timeout: 10s
scrape_configs:
  - job_name: vtorrent-daemon
    static_configs:
      - targets: ['candidate1:22525', 'candidate2:22525', 'candidate3:22525']
        labels:
          environment: $NAME
EOF

if [ "$START_STAKING" = 1 ]; then
    for i in 2 3; do
        port=$((RPC_BASE + i))
        eval "wif=\${STAKER_WIF_$i}"
        imported=$(curl -sf --max-time 15 -H "X-Api-Key: $API_KEY" \
            -H 'Content-Type: application/json' \
            -d "{\"wif\":\"$wif\",\"passphrase\":\"$STAKER_PASSPHRASE\"}" \
            "http://127.0.0.1:${port}/api/v1/wallet/import") || {
            echo "error: wallet import failed on node $i" >&2
            exit 1
        }
        address=$(printf '%s' "$imported" | sed -n 's/.*"address":"\([^"]*\)".*/\1/p')
        if [ -z "$address" ]; then
            echo "error: could not read the imported address from node $i" >&2
            exit 1
        fi

        faucet=$(curl -sf --max-time 30 -H "X-Api-Key: $API_KEY" \
            -H 'Content-Type: application/json' \
            -d "{\"address\":\"$address\",\"amount_satoshis\":$FAUCET_AMOUNT}" \
            "http://127.0.0.1:$((RPC_BASE + 1))/api/v1/faucet") || {
            echo "error: faucet call failed for node $i" >&2
            exit 1
        }
        funded_height=$(printf '%s' "$faucet" | sed -n 's/.*"block_height":\([0-9]*\).*/\1/p')

        for _ in $(seq 1 60); do
            heights=$(for n in 1 2 3; do
                curl -sf --max-time 5 -H "X-Api-Key: $API_KEY" \
                    "http://127.0.0.1:$((RPC_BASE + n))/api/v1/info" \
                    | sed -n 's/.*"block_height":\([0-9]*\).*/\1/p'
            done)
            behind=0
            for h in $heights; do
                [ "${h:-0}" -ge "${funded_height:-0}" ] || behind=1
            done
            [ "$behind" -eq 0 ] && break
            sleep 1
        done

        curl -sf --max-time 15 -H "X-Api-Key: $API_KEY" -H 'Content-Type: application/json' \
            -d "{\"passphrase\":\"$STAKER_PASSPHRASE\",\"timeout_secs\":0}" \
            "http://127.0.0.1:${port}/api/v1/wallet/unlock" >/dev/null || {
            echo "error: wallet unlock failed on node $i" >&2
            exit 1
        }
        curl -sf --max-time 15 -H "X-Api-Key: $API_KEY" -H 'Content-Type: application/json' \
            -d "{\"address\":\"$address\",\"passphrase\":\"$STAKER_PASSPHRASE\"}" \
            "http://127.0.0.1:${port}/api/v1/staking/start" >/dev/null || {
            echo "error: staking start failed on node $i" >&2
            exit 1
        }
        printf 'staker%d=%s\n' "$i" "$address" >> "$EVIDENCE/deployment.txt"
        echo "node$i staking as $address"
    done
fi

cat <<EOF
Fleet $NAME is running.
  node1 RPC  http://127.0.0.1:$((RPC_BASE + 1))
  node2 RPC  http://127.0.0.1:$((RPC_BASE + 2))
  node3 RPC  http://127.0.0.1:$((RPC_BASE + 3))
  api key    $SECRET_DIR/api-key
  evidence   $EVIDENCE/deployment.txt
  scrape     $EVIDENCE/prometheus.yml (job vtorrent-daemon)

Tear down with:
  docker rm -f ${NAME}-1 ${NAME}-2 ${NAME}-3 && docker network rm $NAME
EOF
