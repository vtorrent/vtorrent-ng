# Adopt a complete existing torrent download

The torrent engine requires an ownership marker bound to the torrent's info
hash before it writes into an existing directory. Stop the torrent session
before adopting data and retain a backup of the download.

From a checkout of the same release, run:

```bash
cargo run --release -p vtorrent-torrent --bin vtorrent-adopt-torrent -- \
  /path/to/metainfo.torrent /path/to/download-directory
```

The tool reads local metainfo and verifies **every piece hash** against the
existing files before writing or upgrading the marker. It refuses missing or
corrupt pieces, mismatched torrent metadata, symlinked paths, and a marker
belonging to another torrent. It does not write or repair torrent data.

For an incomplete legacy download on Linux, create a **new, nonexistent**
destination directory next to the source and copy only verified pieces:

```bash
cargo run --release -p vtorrent-torrent --bin vtorrent-adopt-torrent -- \
  --partial /path/to/metainfo.torrent /path/to/old-download-directory \
  /path/to/new-download-directory
```

The source is never modified. The tool hashes each complete piece, copies only
matching pieces, writes a resume bitfield, verifies the copied pieces again,
and atomically installs the destination without replacing an existing path.
Missing and corrupt pieces are downloaded normally when the torrent resumes.
Linux `renameat2` is required; other platforms fail closed. Metainfo is limited
to 4 MiB, pieces to 16 MiB, and the migration to one million pieces. Allow
enough disk space for the copy. Do not manually create or copy ownership markers.

The existing path checks do not eliminate a concurrent intermediate-directory
replacement race. Run adoption only in a download tree that other local users
cannot modify.
