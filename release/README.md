# Release dependency lockfile

`release/Cargo.lock` is the reviewed dependency graph for release builds. The
root `Cargo.lock` remains ignored for the workspace-library workflow.

Build the daemon image with this lockfile:

```bash
docker build -f docker/Dockerfile -t vtorrent/node:<tag> .
```

The Dockerfile copies `release/Cargo.lock` to `Cargo.lock` and builds with
`cargo build --release --locked`.

Verify a checkout with:

```bash
cp release/Cargo.lock Cargo.lock
cargo metadata --locked --format-version 1 > /dev/null
```

Regenerate deliberately with `cargo generate-lockfile`, review the dependency
diff, then copy the resulting root `Cargo.lock` to `release/Cargo.lock`.
