// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Core data structures for leader election
//!
//! This module defines the fundamental types used in the ballot-based
//! leader election algorithm, including leader state, candidate info, and configuration.
//!
//! # Design Overview
//!
//! The election algorithm uses a ballot-based approach (similar to Raft's term):
//! - **LeaderState**: Stored at a single key, contains ballot number and leader identity
//! - **CandidateInfo**: Per-candidate registration with versionstamp for ordering
//! - **Ballot Numbers**: Monotonically increasing, higher ballot always wins

use std::time::Duration;

/// Default lease duration for leadership
pub const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(10);

/// Default heartbeat interval (should be lease_duration / 3 approximately)
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);

/// Default candidate timeout (when to consider a candidate dead)
pub const DEFAULT_CANDIDATE_TIMEOUT: Duration = Duration::from_secs(15);

/// The core leader state - stored at a single key
///
/// Contains all information
/// needed to determine leadership without scanning candidates.
///
/// # Ballot Numbers
///
/// The ballot number works like Raft's term:
/// - Monotonically increasing counter
/// - Higher ballot always wins
/// - Prevents split-brain after recovery/partition
/// - Incremented on every leadership claim or refresh
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderState {
    /// Ballot number (like Raft's term)
    ///
    /// Always increments when claiming or refreshing leadership.
    /// Higher ballot wins in any conflict.
    pub ballot: u64,

    /// Unique identifier of the leader process
    pub leader_id: String,

    /// Leader's priority (higher = more preferred)
    ///
    /// Used for preemption decisions when `allow_preemption` is enabled.
    pub priority: i32,

    /// Absolute timestamp when lease expires (nanos since epoch)
    ///
    /// Leadership is only valid while `current_time < lease_expiry_nanos`.
    pub lease_expiry_nanos: u64,

    /// Versionstamp assigned when this process registered
    ///
    /// Used for identity consistency and ordering.
    pub versionstamp: [u8; 12],
}

impl LeaderState {
    /// Check if the lease is still valid
    ///
    /// # Arguments
    /// * `current_time` - Current time as Duration since epoch
    ///
    /// # Returns
    /// `true` if the lease has not expired
    pub fn is_lease_valid(&self, current_time: Duration) -> bool {
        current_time.as_nanos() < self.lease_expiry_nanos as u128
    }

    /// Get remaining lease duration, if any
    ///
    /// # Arguments
    /// * `current_time` - Current time as Duration since epoch
    ///
    /// # Returns
    /// `Some(Duration)` if lease is still valid, `None` if expired
    pub fn remaining_lease(&self, current_time: Duration) -> Option<Duration> {
        let current_nanos = current_time.as_nanos() as u64;
        if current_nanos < self.lease_expiry_nanos {
            Some(Duration::from_nanos(
                self.lease_expiry_nanos - current_nanos,
            ))
        } else {
            None
        }
    }
}

/// Information about a registered candidate
///
/// Candidates exist independently of leadership. A process must register
/// as a candidate before it can claim leadership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInfo {
    /// Unique identifier for the process
    pub process_id: String,

    /// Candidate's priority for leader selection
    ///
    /// Higher priority candidates can preempt lower priority leaders
    /// when preemption is enabled.
    pub priority: i32,

    /// Last heartbeat timestamp (nanos since epoch)
    ///
    /// Used to determine if candidate is still alive.
    pub last_heartbeat_nanos: u64,

    /// Versionstamp from registration
    ///
    /// Fixed at registration time, never changes on heartbeat.
    /// Provides global ordering of candidates.
    pub versionstamp: [u8; 12],
}

impl CandidateInfo {
    /// Check if this candidate is still alive
    ///
    /// # Arguments
    /// * `current_time` - Current time as Duration since epoch
    /// * `timeout` - Maximum time since last heartbeat
    ///
    /// # Returns
    /// `true` if the candidate has sent a heartbeat within the timeout
    pub fn is_alive(&self, current_time: Duration, timeout: Duration) -> bool {
        let last_seen = Duration::from_nanos(self.last_heartbeat_nanos);
        current_time.saturating_sub(last_seen) < timeout
    }
}

/// Result of an election cycle
///
/// Returned by `run_election_cycle` to indicate whether this process
/// is the leader or a follower.
#[derive(Debug, Clone)]
pub enum ElectionResult {
    /// This process is the leader
    Leader(LeaderState),

    /// This process is a follower
    ///
    /// Contains the current leader state if one exists.
    Follower(Option<LeaderState>),
}

impl ElectionResult {
    /// Check if this result indicates leadership
    pub fn is_leader(&self) -> bool {
        matches!(self, ElectionResult::Leader(_))
    }

    /// Get the leader state, regardless of whether we are leader or follower
    pub fn leader_state(&self) -> Option<&LeaderState> {
        match self {
            ElectionResult::Leader(state) => Some(state),
            ElectionResult::Follower(Some(state)) => Some(state),
            ElectionResult::Follower(None) => None,
        }
    }
}

/// Global configuration for the leader election system
///
/// Controls the behavior of the election system including lease duration,
/// timeouts, and preemption policy.
#[derive(Debug, Clone)]
pub struct ElectionConfig {
    /// How long a leadership lease lasts
    ///
    /// Leader must refresh before this expires to maintain leadership.
    /// Longer values reduce election traffic but slow failover.
    pub lease_duration: Duration,

    /// Recommended heartbeat interval
    ///
    /// Candidates and leaders should send heartbeats at this interval.
    /// Typically `lease_duration / 3` to ensure timely refresh.
    pub heartbeat_interval: Duration,

    /// How long before a candidate is considered dead
    ///
    /// Candidates that haven't sent a heartbeat within this duration
    /// may be evicted from the candidate list.
    pub candidate_timeout: Duration,

    /// Master switch to enable/disable elections
    ///
    /// When disabled:
    /// - No new leaders can be elected
    /// - Existing leader remains until lease expires
    /// - Registration and heartbeats return errors
    pub election_enabled: bool,

    /// Whether higher priority processes can preempt current leader
    ///
    /// If `true`, a candidate with higher priority can take over
    /// leadership even if the current leader's lease is valid.
    /// If `false`, must wait for lease to expire.
    pub allow_preemption: bool,
}

impl Default for ElectionConfig {
    /// Creates default configuration with sensible production values
    ///
    /// - 10 second lease duration
    /// - 3 second heartbeat interval
    /// - 15 second candidate timeout
    /// - Elections enabled
    /// - Preemption enabled
    fn default() -> Self {
        Self {
            lease_duration: DEFAULT_LEASE_DURATION,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            candidate_timeout: DEFAULT_CANDIDATE_TIMEOUT,
            election_enabled: true,
            allow_preemption: true,
        }
    }
}

impl ElectionConfig {
    /// Create a new ElectionConfig with custom lease duration
    ///
    /// Other values are derived from the lease duration:
    /// - heartbeat_interval = lease_duration / 3
    /// - candidate_timeout = lease_duration * 1.5
    pub fn with_lease_duration(lease_duration: Duration) -> Self {
        Self {
            lease_duration,
            heartbeat_interval: lease_duration / 3,
            candidate_timeout: lease_duration + lease_duration / 2,
            election_enabled: true,
            allow_preemption: true,
        }
    }
}
