# Release signing

The release workflow publishes `SHA256SUMS` for every release asset. That is an
integrity check, not provenance: anyone who can replace an asset can replace
the checksum file too. A minisign signature over `SHA256SUMS` closes that gap
when the public key is published separately.

## What the workflow does

`release-checksums.yml` always generates and verifies `SHA256SUMS`. If the
repository secret `MINISIGN_SECRET_KEY` is set, it also:

1. requires `release/minisign.pub` to be committed;
2. signs `SHA256SUMS` with the CI secret;
3. uploads `SHA256SUMS.minisig`.

Without the secret, releases remain unsigned and the release notes say so.
Once `release/minisign.pub` is committed, a missing secret is a hard failure
rather than a silent downgrade to unsigned.

## Key setup (operator, offline)

Generate a dedicated release key on an offline machine:

```bash
minisign -G -p release/minisign.pub -s /secure/offline/minisign.key
```

- Commit only `release/minisign.pub`.
- Store the secret key contents in the GitHub Actions secret
  `MINISIGN_SECRET_KEY`. The CI key must be **unencrypted** (minisign would
  otherwise prompt for a passphrase and hang the job). If you keep the key
  encrypted, sign releases locally instead and upload the signature by hand.
- Back up the secret key offline. Losing it only prevents new signatures; it
  does not invalidate old releases. Compromising it invalidates the trust
  chain, so rotate the public key and announce the change.

## Verify a release

```bash
# With a signature and the published public key
scripts/verify-release.sh ./release-assets release/minisign.pub --require-signature

# Integrity only (current unsigned releases)
scripts/verify-release.sh ./release-assets
```

The script checks every entry in `SHA256SUMS` and, when requested, verifies
`SHA256SUMS.minisig` against the public key before trusting any asset.

## Platform code signing

Minisign does not make macOS or Windows trust the installer. Those require:

- **macOS:** a Developer ID Application certificate and notarization
  credentials (`signingIdentity` in `vtorrent-tauri/tauri.conf.json`).
- **Windows:** an Authenticode certificate (`certificateThumbprint`).

Both are separate operator key-management tasks. Until they exist, the release
notes must continue to say the installers are unsigned.
