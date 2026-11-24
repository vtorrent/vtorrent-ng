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
rather than a silent downgrade to unsigned. A repository variable
`VTR_REQUIRE_SIGNING=1` enforces the same rule even for tags that predate the
public key.

## Key setup (operator, offline)

Generate a dedicated release key on an offline machine:

```bash
scripts/generate-release-key.sh /secure/offline/vtorrent-release-keys
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

## Trust boundary and CI protection

The checksum job does not build the desktop assets; it hashes the files attached
to the published release. Minisign authenticates the contents of `SHA256SUMS` at
signing time, not who built the assets. The workflow therefore:

- runs in the protected GitHub `release` environment, so publishing requires
  the environment's reviewers/deployment rules; and
- verifies that the release tag commit is an ancestor of `origin/main` before
  generating or signing checksums.

These checks stop a stray token from publishing a release from an unmerged
commit, but they do not prove that an attached installer came from the
`build-desktop` job. Until same-run build attestation is added, the release
operator must attach only assets produced by `build-desktop` for the same tag
and commit, and record the workflow run ID with the release. The daemon image is
built separately by the `docker-image` CI job from `release/Cargo.lock`; its
image ID is the comparison point for the local clean rebuild.

## Platform code signing

Minisign does not make macOS or Windows trust the installer. Those require:

- **macOS:** a Developer ID Application certificate and notarization
  credentials (`signingIdentity` in `vtorrent-tauri/tauri.conf.json`).
- **Windows:** an Authenticode certificate (`certificateThumbprint`).

Both are separate operator key-management tasks. Until they exist, the release
notes must continue to say the installers are unsigned.
