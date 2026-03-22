//! vtorrent-rpc: HTTP JSON-RPC API server for the vTorrent node.
//!
//! Exposes a REST-style JSON API on localhost:22525 that the Tauri UI
//! and external tools can use to interact with the running node.
//!
//! Endpoints:
//! - GET  /api/v1/info              — Node info and chain status
//! - GET  /api/v1/blockchain/height — Current block height
//! - GET  /api/v1/blockchain/block/:hash — Get block by hash
//! - GET  /api/v1/mempool           — Mempool transactions
//! - GET  /api/v1/wallet/balance    — Wallet balance
//! - GET  /api/v1/wallet/addresses  — Wallet addresses
//! - POST /api/v1/wallet/send       — Send VTR
//! - POST /api/v1/wallet/unlock     — Unlock wallet
//! - POST /api/v1/wallet/lock       — Lock wallet
//! - GET  /api/v1/staking/status    — Staking status
//! - POST /api/v1/staking/start     — Start staking
//! - POST /api/v1/staking/stop      — Stop staking
//! - GET  /api/v1/torrent/sessions  — Active torrent sessions
//! - POST /api/v1/torrent/add       — Add a torrent (magnet or .torrent)
//! - DELETE /api/v1/torrent/:id     — Remove a torrent session
//! - GET  /api/v1/dex/orders        — DEX order book
//! - POST /api/v1/dex/order         — Place a DEX order
//! - DELETE /api/v1/dex/order/:id   — Cancel a DEX order

pub mod error;
pub mod handlers;
pub mod models;
pub mod server;
pub mod state;
pub mod ws;
pub mod metrics;
