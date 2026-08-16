//! vtorrent-torrent: BitTorrent integration with VTR incentive payments.
//!
//! This crate provides:
//! - Torrent metadata parsing (BEP-3 .torrent files and magnet links)
//! - Tracker announce/scrape (HTTP and UDP trackers)
//! - Peer wire protocol session management
//! - VTR incentive payment logic (earn for seeding, pay for leeching)
//! - Upload/download bandwidth accounting per peer

pub mod error;
pub mod incentive;
pub mod metainfo;
pub mod peer_wire;
pub mod session;
pub mod tracker;
