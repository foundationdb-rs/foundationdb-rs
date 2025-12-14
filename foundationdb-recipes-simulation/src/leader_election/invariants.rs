//! Invariant verification methods for leader election simulation.
//!
//! Contains all 8 invariant checks:
//! 1. Log Entry Completeness
//! 2. Timestamp Monotonicity
//! 3. Safety (No Overlapping Leadership)
//! 4. Ballot Conservation
//! 5. Candidate Registration Consistency
//! 6. Leader Is Registered Candidate
//! 7. Operation Sequencing
//! 8. Error Rate Bounds

use std::collections::BTreeMap;
use std::time::Duration;

use super::types::{
    get_lease_duration, CheckResult, ClientStats, DatabaseSnapshot, LogEntries, OpType,
    MAX_ERROR_RATE, OP_REGISTER, OP_TRY_BECOME_LEADER,
};
use super::LeaderElectionWorkload;

impl LeaderElectionWorkload {
    /// Invariant 1: Log Entry Completeness
    /// - Every client should have logged operations
    /// - Op numbers should be sequential per client (0, 1, 2, ...)
    pub(crate) fn verify_log_completeness(
        &self,
        _entries: &LogEntries,
        stats: &BTreeMap<i32, ClientStats>,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Check all clients logged something
        for client_id in 0..self.client_count {
            if !stats.contains_key(&client_id) {
                issues.push(format!("Client {} has no log entries", client_id));
            }
        }

        // Check op_nums are sequential per client
        for (client_id, client_stats) in stats {
            let mut sorted_ops = client_stats.op_nums.clone();
            sorted_ops.sort();

            for (i, op_num) in sorted_ops.iter().enumerate() {
                if *op_num != i as u64 {
                    issues.push(format!(
                        "Client {}: op_num gap at index {}, expected {}, got {}",
                        client_id, i, i, op_num
                    ));
                    break;
                }
            }
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "All {} clients logged sequential operations",
                    stats.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 2: Timestamp Monotonicity Per Client
    /// - Each client's log entries should have monotonically increasing timestamps
    pub(crate) fn verify_timestamp_monotonicity(&self, entries: &LogEntries) -> (bool, String) {
        // Group entries by client
        let mut client_entries: BTreeMap<i32, Vec<(u64, f64)>> = BTreeMap::new();

        for ((_, client_id, op_num), (_, _, timestamp, _)) in entries {
            client_entries
                .entry(*client_id)
                .or_default()
                .push((*op_num, *timestamp));
        }

        let mut issues = Vec::new();

        for (client_id, mut ops) in client_entries {
            // Sort by op_num to get chronological order
            ops.sort_by_key(|(op_num, _)| *op_num);

            let mut prev_timestamp: Option<f64> = None;
            for (op_num, timestamp) in ops {
                if let Some(prev) = prev_timestamp {
                    if timestamp < prev {
                        issues.push(format!(
                            "Client {}: timestamp decreased at op_num {} ({} < {})",
                            client_id, op_num, timestamp, prev
                        ));
                    }
                }
                prev_timestamp = Some(timestamp);
            }
        }

        if issues.is_empty() {
            (true, "All client timestamps are monotonically increasing".to_string())
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 3: Safety - No Overlapping Leadership
    /// - If client A became leader at time T1 with lease L, and client B became leader at T2
    /// - Their lease periods should NOT overlap (unless same client refreshing)
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
        leadership_periods.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut issues = Vec::new();

        // Check for overlaps between different clients
        for i in 0..leadership_periods.len() {
            for j in (i + 1)..leadership_periods.len() {
                let (start_i, client_i, end_i) = leadership_periods[i];
                let (start_j, client_j, _end_j) = leadership_periods[j];

                // Different clients with overlapping periods
                if client_i != client_j && start_j < end_i {
                    issues.push(format!(
                        "SAFETY VIOLATION: Client {} (lease {:.3}-{:.3}) overlaps with Client {} (start {:.3})",
                        client_i, start_i, end_i, client_j, start_j
                    ));
                }
            }
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

    /// Invariant 4: Ballot Conservation Law (AtomicOps-style dual accumulation)
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

    /// Invariant 5: Candidate Registration Consistency
    /// - All clients that sent heartbeats should be registered as candidates
    pub(crate) fn verify_candidate_consistency(
        &self,
        stats: &BTreeMap<i32, ClientStats>,
        snapshot: &DatabaseSnapshot,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Build set of registered candidate process_ids
        let registered: std::collections::BTreeSet<String> = snapshot
            .candidates
            .iter()
            .map(|c| c.process_id.clone())
            .collect();

        // Check all clients that heartbeated are registered
        for (client_id, client_stats) in stats {
            if client_stats.heartbeat_count > 0 {
                let process_id = format!("process_{}", client_id);
                if !registered.contains(&process_id) {
                    issues.push(format!(
                        "Client {} sent {} heartbeats but not in candidate list",
                        client_id, client_stats.heartbeat_count
                    ));
                }
            }
        }

        // Check candidates have non-zero versionstamps
        for candidate in &snapshot.candidates {
            if candidate.versionstamp == [0u8; 12] {
                issues.push(format!(
                    "Candidate {} has zero versionstamp",
                    candidate.process_id
                ));
            }
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "All {} candidates are consistent",
                    snapshot.candidates.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 6: Leader Is Registered Candidate
    /// - If there's a current leader, they must exist in the candidates list
    pub(crate) fn verify_leader_is_candidate(&self, snapshot: &DatabaseSnapshot) -> (bool, String) {
        match &snapshot.leader_state {
            Some(leader) => {
                let leader_in_candidates = snapshot
                    .candidates
                    .iter()
                    .any(|c| c.process_id == leader.leader_id);

                if leader_in_candidates {
                    // Also verify versionstamp matches
                    let candidate = snapshot
                        .candidates
                        .iter()
                        .find(|c| c.process_id == leader.leader_id);

                    if let Some(c) = candidate {
                        if c.versionstamp == leader.versionstamp {
                            (
                                true,
                                format!(
                                    "Leader {} is registered candidate with matching versionstamp",
                                    leader.leader_id
                                ),
                            )
                        } else {
                            (
                                false,
                                format!(
                                    "Leader {} versionstamp mismatch: leader={:?}, candidate={:?}",
                                    leader.leader_id, leader.versionstamp, c.versionstamp
                                ),
                            )
                        }
                    } else {
                        (false, format!("Leader {} not found in candidates", leader.leader_id))
                    }
                } else {
                    (
                        false,
                        format!("Leader {} is NOT in candidate list", leader.leader_id),
                    )
                }
            }
            None => (true, "No leader to verify".to_string()),
        }
    }

    /// Invariant 7: Operation Sequencing
    /// - Each client should have exactly one registration (first operation)
    /// - Registrations should happen before heartbeats
    pub(crate) fn verify_operation_sequencing(
        &self,
        entries: &LogEntries,
        stats: &BTreeMap<i32, ClientStats>,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Check each client has exactly one registration
        for (client_id, client_stats) in stats {
            if client_stats.register_count != 1 {
                issues.push(format!(
                    "Client {} has {} registrations (expected 1)",
                    client_id, client_stats.register_count
                ));
            }
        }

        // Check registration is first op for each client
        let mut client_first_op: BTreeMap<i32, (u64, i64)> = BTreeMap::new();
        for ((_, client_id, op_num), (op_type, _, _, _)) in entries {
            let entry = client_first_op.entry(*client_id).or_insert((*op_num, *op_type));
            if *op_num < entry.0 {
                *entry = (*op_num, *op_type);
            }
        }

        for (client_id, (first_op_num, first_op_type)) in &client_first_op {
            if *first_op_type != OP_REGISTER {
                let op_name = OpType::from_i64(*first_op_type)
                    .map(|o| o.as_str())
                    .unwrap_or("Unknown");
                issues.push(format!(
                    "Client {}: first operation is {} (op_num={}), not Register",
                    client_id, op_name, first_op_num
                ));
            }
        }

        if issues.is_empty() {
            (
                true,
                "All clients have correct operation sequencing".to_string(),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 8: Error Rate Bounds
    /// - Error rate should be below threshold
    pub(crate) fn verify_error_rate(&self, stats: &BTreeMap<i32, ClientStats>) -> (bool, String) {
        let total_ops: usize = stats
            .values()
            .map(|s| s.register_count + s.heartbeat_count + s.leadership_attempt_count)
            .sum();
        let total_errors: usize = stats.values().map(|s| s.error_count).sum();

        if total_ops == 0 {
            return (true, "No operations to check".to_string());
        }

        let error_rate = total_errors as f64 / total_ops as f64;

        if error_rate <= MAX_ERROR_RATE {
            (
                true,
                format!(
                    "Error rate {:.2}% is within threshold ({:.0}%)",
                    error_rate * 100.0,
                    MAX_ERROR_RATE * 100.0
                ),
            )
        } else {
            (
                false,
                format!(
                    "Error rate {:.2}% EXCEEDS threshold ({:.0}%): {}/{} ops failed",
                    error_rate * 100.0,
                    MAX_ERROR_RATE * 100.0,
                    total_errors,
                    total_ops
                ),
            )
        }
    }

    /// Run all invariant checks and return results
    pub(crate) fn run_all_invariant_checks(
        &self,
        entries: &LogEntries,
        stats: &BTreeMap<i32, ClientStats>,
        snapshot: &DatabaseSnapshot,
    ) -> CheckResult {
        let lease_duration = get_lease_duration(snapshot, self.heartbeat_timeout_secs);

        let mut results: Vec<(&'static str, bool, String)> = Vec::new();

        // Run each invariant check
        let (pass, detail) = self.verify_log_completeness(entries, stats);
        results.push(("LogCompleteness", pass, detail));

        let (pass, detail) = self.verify_timestamp_monotonicity(entries);
        results.push(("TimestampMonotonicity", pass, detail));

        let (pass, detail) = self.verify_no_overlapping_leadership(entries, lease_duration);
        results.push(("NoOverlappingLeadership", pass, detail));

        let (pass, detail) = self.verify_ballot_conservation(entries, snapshot);
        results.push(("BallotConservation", pass, detail));

        let (pass, detail) = self.verify_candidate_consistency(stats, snapshot);
        results.push(("CandidateConsistency", pass, detail));

        let (pass, detail) = self.verify_leader_is_candidate(snapshot);
        results.push(("LeaderIsCandidate", pass, detail));

        let (pass, detail) = self.verify_operation_sequencing(entries, stats);
        results.push(("OperationSequencing", pass, detail));

        let (pass, detail) = self.verify_error_rate(stats);
        results.push(("ErrorRate", pass, detail));

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
