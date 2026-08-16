pub mod block_parser;
/// vTorrent Snapshot Extractor
///
/// This crate reads the legacy vTorrent blockchain data (stored in LevelDB
/// format, identical to Bitcoin Core's chainstate) and produces a compact,
/// cryptographically-signed UTXO snapshot file.
///
/// The snapshot is used in two ways:
/// 1. Embedded in the new chain's genesis block to credit all legacy holders.
/// 2. Queried by the wallet import wizard to show users their claimable balance.
pub mod error;
pub mod leveldb_reader;
pub mod snapshot_reader;
pub mod snapshot_writer;
pub mod utxo_set;
