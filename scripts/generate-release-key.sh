#!/usr/bin/env bash
# Generate a release minisign keypair in an owner-only directory.
#
# Run this on an offline/trusted machine, then move the secret key to offline
# storage and put its contents in the GitHub secret MINISIGN_SECRET_KEY.
#
#   scripts/generate-release-key.sh [output-dir]
#
# The secret key is generated unencrypted because CI cannot answer a
# passphrase prompt. Treat the file as a high-value credential.
set -euo pipefail

out="${1:-$HOME/.config/vtorrent/release-keys}"
umask 077

if ! command -v minisign >/dev/null 2>&1; then
    echo "minisign is required. Install it from your package manager or" >&2
    echo "https://jedisct1.github.io/minisign/ and re-run this script." >&2
    exit 1
fi

mkdir -p "$out"
chmod 700 "$out"
if [[ -e "$out/minisign.key" || -e "$out/minisign.pub" ]]; then
    echo "refusing to overwrite an existing key in $out" >&2
    exit 1
fi

minisign -G -W -p "$out/minisign.pub" -s "$out/minisign.key"
chmod 600 "$out/minisign.key"
chmod 644 "$out/minisign.pub"

cat <<EOF

Keypair written to:
  public:  $out/minisign.pub
  secret:  $out/minisign.key

Next steps:
  1. Commit only the public key:  cp "$out/minisign.pub" release/minisign.pub
  2. Add the secret key file contents to the GitHub Actions secret
     MINISIGN_SECRET_KEY (the workflow requires it once the public key is
     committed).
  3. Back up the secret key offline, then remove it from this host:
     shred -u "$out/minisign.key"
  4. Test a signature with scripts/verify-release.sh after the next release.

Never commit minisign.key, paste it into a chat, or leave it on a shared host.
EOF
