//! # vtorrent-script
//!
//! A stack-based script execution engine for vTorrent, supporting:
//!
//! - **P2PKH** (Pay-to-Public-Key-Hash) — the standard output type
//! - **P2PK** (Pay-to-Public-Key) — legacy output type
//! - **P2SH** (Pay-to-Script-Hash) — allows complex scripts behind a hash
//! - **P2MS** (Pay-to-Multisig) — M-of-N multisignature outputs
//! - **HTLC** (Hash Time-Locked Contract) — atomic swap scripts
//! - **OP_RETURN** — provably unspendable data outputs
//!
//! The engine is a minimal subset of Bitcoin Script, using the same opcodes
//! and stack semantics. This ensures compatibility with existing tooling and
//! allows future extension to full Script support.

pub mod error;
pub mod opcode;
pub mod script;
pub mod engine;
pub mod standard;

pub use error::ScriptError;
pub use opcode::Opcode;
pub use script::Script;
pub use engine::{Engine, ScriptEnv};
pub use standard::{ScriptType, classify_script, build_p2pkh, build_p2sh, build_p2ms, build_htlc, build_op_return};
