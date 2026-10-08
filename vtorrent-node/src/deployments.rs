//! BIP-9-style versionbits deployments for coordinated consensus activation.
//!
//! A deployment reserves a bit in the block header's `version` field. Miners
//! signal readiness by setting the bit; when a supermajority of a retarget
//! period signals, the deployment locks in and activates in the next period.
//! See `docs/network-upgrade-design.md`.

use serde::{Deserialize, Serialize};

/// A named consensus deployment (BIP-9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deployment {
    pub name: String,
    /// The versionbits bit (0..=28) this deployment uses.
    pub bit: u8,
    /// Height at which signalling begins.
    pub start_height: u32,
    /// Height after which the deployment fails if not locked in.
    pub timeout_height: u32,
}

/// The state of a deployment at a given tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    /// Before `start_height`.
    Defined,
    /// Signalling in progress.
    Started,
    /// A period reached the threshold; activates next period.
    LockedIn,
    /// The rule is active.
    Active,
    /// `timeout_height` passed without lock-in.
    Failed,
}

/// The default retarget period (blocks per signalling window).
pub const DEFAULT_PERIOD: u32 = 2016;
/// The default signalling threshold (percent of a period).
pub const DEFAULT_THRESHOLD_PCT: u32 = 90;

/// Whether `version` signals this deployment's bit.
pub fn signals(deployment: &Deployment, version: u32) -> bool {
    (version >> deployment.bit) & 1 == 1
}

/// Evaluate a deployment's state.
///
/// `signals_per_period[i]` is the number of blocks in period `i` (0-based from
/// genesis) that signalled the bit. Only periods fully below `tip_height` are
/// considered.
pub fn deployment_state(
    deployment: &Deployment,
    period: u32,
    threshold_pct: u32,
    tip_height: u32,
    signals_per_period: &[u32],
) -> DeploymentState {
    if period == 0 {
        return DeploymentState::Defined;
    }
    if tip_height < deployment.start_height {
        return DeploymentState::Defined;
    }
    if tip_height >= deployment.timeout_height {
        // Timeout reached: locked-in before timeout is handled below; if we get
        // here without returning LockedIn/Active, the deployment failed.
        // (We still scan below in case it locked in earlier.)
    }

    let start_period = deployment.start_height / period;
    let timeout_period = deployment.timeout_height / period;
    let tip_period = tip_height / period;

    // Scan completed periods from the start period up to the last completed one.
    let last_completed = tip_period.saturating_sub(1);
    let mut locked_in_period: Option<u32> = None;
    let mut p = start_period;
    while p <= last_completed && p <= timeout_period {
        let signalled = signals_per_period.get(p as usize).copied().unwrap_or(0);
        // A period "signals" if at least `threshold_pct`% of its blocks set the
        // bit. The period's block count is `period` (the last period may be
        // shorter, but we only consider completed periods here).
        if signalled.saturating_mul(100) >= period.saturating_mul(threshold_pct) {
            locked_in_period = Some(p);
            break;
        }
        p += 1;
    }

    match locked_in_period {
        Some(lip) => {
            // LockedIn during period `lip`; Active from period `lip + 1`.
            if tip_period > lip {
                DeploymentState::Active
            } else {
                DeploymentState::LockedIn
            }
        }
        None => {
            if tip_height >= deployment.timeout_height {
                DeploymentState::Failed
            } else {
                DeploymentState::Started
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep() -> Deployment {
        Deployment {
            name: "test".into(),
            bit: 0,
            start_height: 100,
            timeout_height: 10_000,
        }
    }

    #[test]
    fn signals_reads_the_bit() {
        let d = dep();
        assert!(signals(&d, 0b1));
        assert!(!signals(&d, 0b0));
        let d2 = Deployment { bit: 3, ..dep() };
        assert!(signals(&d2, 0b1000));
        assert!(!signals(&d2, 0b1));
    }

    #[test]
    fn defined_before_start() {
        let d = dep();
        assert_eq!(
            deployment_state(&d, 100, 90, 50, &[]),
            DeploymentState::Defined
        );
    }

    #[test]
    fn started_while_signalling() {
        let d = dep();
        // tip in period 1 (100..199), start period 1, no completed period yet.
        assert_eq!(
            deployment_state(&d, 100, 90, 150, &[0, 0]),
            DeploymentState::Started
        );
    }

    #[test]
    fn locks_in_then_activates() {
        let d = dep();
        // Period 1 (100..199) fully signals; tip in period 2 → Active.
        let signals = vec![0, 100, 0];
        assert_eq!(
            deployment_state(&d, 100, 90, 250, &signals),
            DeploymentState::Active
        );
        // Tip still in period 1 (not completed) → Started.
        assert_eq!(
            deployment_state(&d, 100, 90, 150, &[0, 100]),
            DeploymentState::Started
        );
    }

    #[test]
    fn fails_after_timeout_without_lockin() {
        let d = dep();
        let signals = vec![0; 200];
        assert_eq!(
            deployment_state(&d, 100, 90, 10_000, &signals),
            DeploymentState::Failed
        );
    }

    #[test]
    fn below_threshold_does_not_lock_in() {
        let d = dep();
        // 89% < 90% threshold.
        let signals = vec![0, 89, 0];
        assert_eq!(
            deployment_state(&d, 100, 90, 250, &signals),
            DeploymentState::Started
        );
    }
}
