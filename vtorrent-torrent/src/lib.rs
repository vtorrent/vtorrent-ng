//! vtorrent-torrent: BitTorrent integration with VTR incentive payments.
//!
//! This crate provides:
//! - Torrent metadata parsing (BEP-3 .torrent files and magnet links)
//! - Tracker announce/scrape (HTTP and UDP trackers)
//! - Peer wire protocol session management
//! - VTR incentive payment logic (earn for seeding, pay for leeching)
//! - Upload/download bandwidth accounting per peer

pub mod bencode_guard;
pub mod dht;
pub mod engine;
pub mod error;
pub mod incentive;
pub mod metadata;
pub mod metainfo;
pub mod payment;
pub mod peer_wire;
pub mod scheduler;
pub mod session;
pub mod tracker;
pub mod udp;
