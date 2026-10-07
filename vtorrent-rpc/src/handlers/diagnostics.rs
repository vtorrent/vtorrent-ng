//! Node diagnostics ("doctor"): read-only health checks.
//!
//! Reuses the node's own state (no second implementation of integrity checks).
//! See `docs/node-diagnostics-design.md`.

use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

use crate::error::RpcResult;
use crate::state::AppState;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    /// Suggested remediation when not `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsReport {
    pub checks: Vec<DiagnosticCheck>,
    /// Overall verdict: `ok` if no check failed, else `fail`.
    pub verdict: CheckStatus,
    pub as_of_height: u64,
}

/// GET /api/v1/diagnostics
pub async fn get_diagnostics(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<DiagnosticsReport>> {
    let mut checks = Vec::new();

    // ── Chain tip + UTXO commitment consistency ───────────────────────────────
    let (height, commitment_ok, commitment_detail) = {
        let mut chain = state.chain.lock().await;
        let height = chain.best_height();
        match chain.verify_utxo_commitment() {
            Some(true) => (height, true, "tip utxo_root matches recomputation".into()),
            Some(false) => (
                height,
                false,
                "tip utxo_root does NOT match recomputation — chain state may be corrupt".into(),
            ),
            None => (
                height,
                false,
                "could not read the tip UTXO commitment".into(),
            ),
        }
    };
    checks.push(if commitment_ok {
        DiagnosticCheck {
            name: "utxo_commitment".into(),
            status: CheckStatus::Ok,
            detail: commitment_detail,
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            name: "utxo_commitment".into(),
            status: CheckStatus::Fail,
            detail: commitment_detail,
            remediation: Some("run a reindex (see docs/reindex-rescan-design.md)".into()),
        }
    });

    // ── Peers ─────────────────────────────────────────────────────────────────
    let peers = *state.peer_count.read().await;
    checks.push(if peers > 0 {
        DiagnosticCheck {
            name: "peers".into(),
            status: CheckStatus::Ok,
            detail: format!("{peers} peer connection(s)"),
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            name: "peers".into(),
            status: CheckStatus::Warn,
            detail: "no peer connections".into(),
            remediation: Some("check connectivity / seeds; see docs/dns-seeds.md".into()),
        }
    });

    // ── Sync progress ─────────────────────────────────────────────────────────
    let best_peer = *state.best_peer_height.read().await;
    checks.push(if best_peer == 0 || height as u64 + 2 >= best_peer {
        DiagnosticCheck {
            name: "sync".into(),
            status: CheckStatus::Ok,
            detail: format!("at tip (height {height}, best peer {best_peer})"),
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            name: "sync".into(),
            status: CheckStatus::Warn,
            detail: format!("behind: height {height}, best peer {best_peer}"),
            remediation: Some("allow sync to continue; check peers".into()),
        }
    });

    // ── Wallet ────────────────────────────────────────────────────────────────
    let has_wallet = state.wallet_encrypted.read().await.is_some();
    let unlocked = state.is_wallet_unlocked().await;
    checks.push(if has_wallet {
        DiagnosticCheck {
            name: "wallet".into(),
            status: CheckStatus::Ok,
            detail: if unlocked {
                "wallet imported and unlocked".into()
            } else {
                "wallet imported (locked)".into()
            },
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            name: "wallet".into(),
            status: CheckStatus::Warn,
            detail: "no wallet imported".into(),
            remediation: Some("import a wallet to send/stake".into()),
        }
    });

    // ── Mempool ───────────────────────────────────────────────────────────────
    let (mempool_size, mempool_bytes) = {
        let mp = state.mempool.lock().await;
        (mp.size(), mp.total_bytes())
    };
    checks.push(DiagnosticCheck {
        name: "mempool".into(),
        status: CheckStatus::Ok,
        detail: format!("{mempool_size} tx(s), {mempool_bytes} bytes"),
        remediation: None,
    });

    let verdict = if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        CheckStatus::Fail
    } else {
        CheckStatus::Ok
    };

    Ok(Json(DiagnosticsReport {
        checks,
        verdict,
        as_of_height: height as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_fails_on_any_fail() {
        let checks = [
            DiagnosticCheck {
                name: "a".into(),
                status: CheckStatus::Ok,
                detail: String::new(),
                remediation: None,
            },
            DiagnosticCheck {
                name: "b".into(),
                status: CheckStatus::Fail,
                detail: String::new(),
                remediation: None,
            },
        ];
        let verdict = if checks.iter().any(|c| c.status == CheckStatus::Fail) {
            CheckStatus::Fail
        } else {
            CheckStatus::Ok
        };
        assert_eq!(verdict, CheckStatus::Fail);
    }
}
