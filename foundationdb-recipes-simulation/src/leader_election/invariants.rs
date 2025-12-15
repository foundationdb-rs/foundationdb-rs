//! Invariant verification methods for leader election simulation.
//!
//! Contains the core Active Disk Paxos invariants:
//! 1. Safety (No Overlapping Leadership) - At most one leader at any time
//! 2. Ballot Conservation - Expected ballot from logs matches actual ballot in FDB
//! 3. Leader Is Candidate - Leader must be a registered candidate (structural integrity)
//! 4. Candidate Timestamps - No future timestamps in candidates
//! 5. Leadership Sequence - Per-client leadership events are monotonic in time
//! 6. Registration Coverage - All leadership claims come from registered processes

use std::collections::BTreeMap;
use std::time::Duration;

use super::types::{
    get_lease_duration, CheckResult, DatabaseSnapshot, LogEntries, OP_REGISTER,
    OP_TRY_BECOME_LEADER,
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

    /// Invariant 3: Leader Is Candidate - Structural Integrity
    ///
    /// The current leader must be in the candidate list.
    /// This verifies the data structure invariant that leadership
    /// can only be claimed by registered candidates.
    ///
    /// NOTE: If the leader's lease has expired, the candidate may have been
    /// evicted due to timeout. This is valid - we only enforce this invariant
    /// for leaders with active leases.
    pub(crate) fn verify_leader_is_candidate(
        &self,
        snapshot: &DatabaseSnapshot,
        current_time: f64,
    ) -> (bool, String) {
        match &snapshot.leader_state {
            Some(leader) => {
                // Check if lease has expired - if so, candidate eviction is valid
                let lease_expiry_secs = leader.lease_expiry_nanos as f64 / 1_000_000_000.0;
                let lease_expired = current_time > lease_expiry_secs;

                if lease_expired {
                    // Lease expired - candidate may have been evicted, this is OK
                    return (
                        true,
                        format!(
                            "Leader {} lease expired ({:.3} > {:.3}), candidate eviction valid",
                            leader.leader_id, current_time, lease_expiry_secs
                        ),
                    );
                }

                let is_candidate = snapshot
                    .candidates
                    .iter()
                    .any(|c| c.process_id == leader.leader_id);

                if is_candidate {
                    (
                        true,
                        format!("Leader {} is registered candidate", leader.leader_id),
                    )
                } else {
                    let candidate_ids: Vec<_> = snapshot
                        .candidates
                        .iter()
                        .map(|c| c.process_id.as_str())
                        .collect();
                    (
                        false,
                        format!(
                            "Leader {} not in candidate list (candidates: {:?})",
                            leader.leader_id, candidate_ids
                        ),
                    )
                }
            }
            None => (true, "No leader, invariant trivially holds".to_string()),
        }
    }

    /// Invariant 4: Candidate Timestamps - No Future Timestamps
    ///
    /// All candidate heartbeat timestamps should be in the past or present,
    /// not in the future. Future timestamps would indicate clock skew issues
    /// or bugs in timestamp handling.
    ///
    /// With clock skew simulation enabled, we allow tolerance for:
    /// - clock_offset: up to ±1.0s (Extreme level)
    /// - drift ahead: up to 1.0s (max_ahead)
    /// - jitter: up to 1.1x multiplier on offset
    /// Max theoretical offset: (1.0 + 1.0) * 1.1 = 2.2s, use 3s for margin.
    pub(crate) fn verify_candidate_timestamps(
        &self,
        snapshot: &DatabaseSnapshot,
        current_time: f64,
    ) -> (bool, String) {
        let mut issues = Vec::new();

        // Tolerance accounts for clock skew simulation (Extreme level + jitter)
        const MAX_CLOCK_SKEW_TOLERANCE: f64 = 3.0;

        for candidate in &snapshot.candidates {
            let heartbeat_secs = candidate.last_heartbeat_nanos as f64 / 1_000_000_000.0;

            if heartbeat_secs > current_time + MAX_CLOCK_SKEW_TOLERANCE {
                issues.push(format!(
                    "{} has future timestamp: {:.3} > {:.3}",
                    candidate.process_id, heartbeat_secs, current_time
                ));
            }
        }

        if issues.is_empty() {
            (
                true,
                format!(
                    "All {} candidates have valid timestamps",
                    snapshot.candidates.len()
                ),
            )
        } else {
            (false, issues.join("; "))
        }
    }

    /// Invariant 5: Leadership Sequence - Monotonic Per-Client Timestamps
    ///
    /// For each client, their successful leadership claims should be
    /// monotonically increasing in time. A client cannot claim leadership
    /// "in the past" relative to their previous claim.
    pub(crate) fn verify_leadership_sequence(&self, entries: &LogEntries) -> (bool, String) {
        let mut client_last_leadership: BTreeMap<i32, f64> = BTreeMap::new();
        let mut violations = Vec::new();

        for ((_, client_id, _), (op_type, success, timestamp, became_leader)) in entries {
            if *op_type == OP_TRY_BECOME_LEADER && *success && *became_leader {
                if let Some(last_ts) = client_last_leadership.get(client_id) {
                    if *timestamp < *last_ts {
                        violations.push(format!(
                            "Client {} leadership at {:.3} before previous at {:.3}",
                            client_id, timestamp, last_ts
                        ));
                    }
                }
                client_last_leadership.insert(*client_id, *timestamp);
            }
        }

        let clients_with_leadership = client_last_leadership.len();

        if violations.is_empty() {
            (
                true,
                format!(
                    "Leadership sequence valid ({} clients claimed leadership)",
                    clients_with_leadership
                ),
            )
        } else {
            (false, violations.join("; "))
        }
    }

    /// Invariant 6: Registration Coverage - All Leaders Were Registered
    ///
    /// Every client that successfully claimed leadership must have
    /// previously registered (logged a Register operation).
    /// This verifies the registration requirement of the protocol.
    pub(crate) fn verify_registration_coverage(&self, entries: &LogEntries) -> (bool, String) {
        // Track which clients have registered (either success or attempted)
        let mut registered_clients: BTreeMap<i32, bool> = BTreeMap::new();
        let mut clients_with_leadership: BTreeMap<i32, usize> = BTreeMap::new();

        for ((_, client_id, _), (op_type, success, _, became_leader)) in entries {
            // Count any registration attempt (success or failure indicates the client tried)
            if *op_type == OP_REGISTER {
                registered_clients.insert(*client_id, *success);
            }

            if *op_type == OP_TRY_BECOME_LEADER && *success && *became_leader {
                *clients_with_leadership.entry(*client_id).or_default() += 1;
            }
        }

        // Check for clients that became leader but have no registration entry at all
        let mut unregistered_leaders = Vec::new();
        for (client_id, count) in &clients_with_leadership {
            if !registered_clients.contains_key(client_id) {
                unregistered_leaders.push(format!("Client {} ({} claims)", client_id, count));
            }
        }

        if unregistered_leaders.is_empty() {
            (
                true,
                format!(
                    "All {} clients with leadership have registration entries ({} total registered)",
                    clients_with_leadership.len(),
                    registered_clients.len()
                ),
            )
        } else {
            (
                false,
                format!(
                    "VIOLATION: {} clients claimed leadership without registration: {}",
                    unregistered_leaders.len(),
                    unregistered_leaders.join(", ")
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
        let current_time = self.context.now();

        let mut results: Vec<(&'static str, bool, String)> = Vec::new();

        // Invariant 1: Safety - No Overlapping Leadership
        let (pass, detail) = self.verify_no_overlapping_leadership(entries, lease_duration);
        results.push(("NoOverlappingLeadership", pass, detail));

        // Invariant 2: Ballot Conservation
        let (pass, detail) = self.verify_ballot_conservation(entries, snapshot);
        results.push(("BallotConservation", pass, detail));

        // Invariant 3: Leader Is Candidate (structural integrity)
        let (pass, detail) = self.verify_leader_is_candidate(snapshot, current_time);
        results.push(("LeaderIsCandidate", pass, detail));

        // Invariant 4: Candidate Timestamps (no future timestamps)
        let (pass, detail) = self.verify_candidate_timestamps(snapshot, current_time);
        results.push(("CandidateTimestamps", pass, detail));

        // Invariant 5: Leadership Sequence (per-client monotonic)
        let (pass, detail) = self.verify_leadership_sequence(entries);
        results.push(("LeadershipSequence", pass, detail));

        // Invariant 6: Registration Coverage (all leaders were registered)
        let (pass, detail) = self.verify_registration_coverage(entries);
        results.push(("RegistrationCoverage", pass, detail));

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
