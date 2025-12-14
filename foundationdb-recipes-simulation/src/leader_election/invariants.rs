//! Invariant verification methods for leader election simulation.
//!
//! Contains the 2 core Active Disk Paxos invariants:
//! 1. Safety (No Overlapping Leadership) - At most one leader at any time
//! 2. Ballot Conservation - Expected ballot from logs matches actual ballot in FDB

use std::time::Duration;

use super::types::{
    get_lease_duration, CheckResult, DatabaseSnapshot, LogEntries, OP_TRY_BECOME_LEADER,
};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    /// Invariant 1: Safety - No Overlapping Leadership
    ///
    /// Process events chronologically, tracking current effective lease per client.
    /// When the same client refreshes their lease, the old lease is superseded.
    /// Only flag overlap when a DIFFERENT client claims during another's valid lease.
    pub(crate) fn verify_no_overlapping_leadership(
        &self,
        entries: &LogEntries,
        lease_duration: Duration,
    ) -> (bool, String) {
        // Extract leadership events: (timestamp, client_id, lease_end)
        let mut leadership_periods: Vec<(f64, i32, f64)> = Vec::new();
        let lease_secs = lease_duration.as_secs_f64();

        for ((_, client_id, _), (op_type, success, timestamp, became_leader)) in entries {
            if *op_type == OP_TRY_BECOME_LEADER && *success && *became_leader {
                let lease_end = *timestamp + lease_secs;
                leadership_periods.push((*timestamp, *client_id, lease_end));
            }
        }

        // Sort by start timestamp
        leadership_periods
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut issues = Vec::new();

        // Track current effective lease: (client_id, lease_end)
        // When same client refreshes, we update lease_end (superseding old lease)
        // When different client claims, we check for overlap with current lease
        let mut current_lease: Option<(i32, f64)> = None;

        for (start, client_id, lease_end) in &leadership_periods {
            if let Some((curr_client, curr_end)) = current_lease {
                // Only flag if DIFFERENT client claims during valid lease
                if curr_client != *client_id && *start < curr_end {
                    issues.push(format!(
                        "SAFETY VIOLATION: Client {} (lease until {:.3}) overlaps with Client {} (start {:.3})",
                        curr_client, curr_end, client_id, start
                    ));
                }
            }
            // Update current lease (either same client refresh or new leader)
            current_lease = Some((*client_id, *lease_end));
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "No overlapping leadership periods ({} leadership events checked)",
                    leadership_periods.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 2: Ballot Conservation Law (AtomicOps-style dual accumulation)
    /// - Count successful leadership claims from logs
    /// - Compare to actual ballot in FDB
    pub(crate) fn verify_ballot_conservation(
        &self,
        entries: &LogEntries,
        snapshot: &DatabaseSnapshot,
    ) -> (bool, String) {
        // Count leadership claims from logs (expected ballot increments)
        let expected_ballot: u64 = entries
            .values()
            .filter(|(op_type, success, _, became_leader)| {
                *op_type == OP_TRY_BECOME_LEADER && *success && *became_leader
            })
            .count() as u64;

        // Get actual ballot from FDB
        let actual_ballot = snapshot
            .leader_state
            .as_ref()
            .map(|l| l.ballot)
            .unwrap_or(0);

        if expected_ballot == actual_ballot {
            (
                true,
                format!(
                    "Ballot conservation holds: expected={}, actual={}",
                    expected_ballot, actual_ballot
                ),
            )
        } else {
            (
                false,
                format!(
                    "Ballot conservation VIOLATED: expected={}, actual={}, diff={}",
                    expected_ballot,
                    actual_ballot,
                    (actual_ballot as i64 - expected_ballot as i64).abs()
                ),
            )
        }
    }

    /// Run all invariant checks and return results
    pub(crate) fn run_all_invariant_checks(
        &self,
        entries: &LogEntries,
        snapshot: &DatabaseSnapshot,
    ) -> CheckResult {
        let lease_duration = get_lease_duration(snapshot, self.heartbeat_timeout_secs);

        let mut results: Vec<(&'static str, bool, String)> = Vec::new();

        // Invariant 1: Safety - No Overlapping Leadership
        let (pass, detail) = self.verify_no_overlapping_leadership(entries, lease_duration);
        results.push(("NoOverlappingLeadership", pass, detail));

        // Invariant 2: Ballot Conservation
        let (pass, detail) = self.verify_ballot_conservation(entries, snapshot);
        results.push(("BallotConservation", pass, detail));

        // Log each result
        for (name, passed, detail) in &results {
            if *passed {
                self.trace_invariant_pass(name, detail);
            } else {
                self.trace_invariant_fail(name, "invariant holds", detail);
            }
        }

        let passed = results.iter().filter(|(_, p, _)| *p).count();
        let failed = results.len() - passed;

        self.trace_check_summary(passed, failed);

        CheckResult {
            passed,
            failed,
            results,
        }
    }
}
