// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Leader Election module for FoundationDB
//!
//! This module implements a distributed leader election protocol using FoundationDB
//! as shared memory. The algorithm is inspired by Active Disk Paxos and uses ballot
//! numbers (similar to Raft's term) for consistency.
//!
//! # Key Properties
//!
//! - **Safety**: At most one leader at any time (FDB serializable transactions)
//! - **Liveness**: A correct process eventually becomes leader
//! - **O(1) Operations**: Leadership checks and claims are constant time
//!
//! # Algorithm Overview
//!
//! The election algorithm uses a ballot-based approach:
//!
//! 1. **Candidate Registration**: Each process registers with a unique ID and receives
//!    a versionstamp for ordering. The versionstamp is fixed at registration.
//!
//! 2. **Leadership Claims**: Processes attempt to claim leadership by reading the
//!    leader key and writing with an incremented ballot if eligible.
//!
//! 3. **Lease Refresh**: Leaders must periodically refresh their lease to maintain
//!    leadership. Each refresh increments the ballot.
//!
//! 4. **Preemption**: Higher priority candidates can preempt lower priority leaders
//!    (if enabled in configuration).
//!
//! # Ballot Numbers
//!
//! Ballot numbers work like Raft's term:
//! - Monotonically increasing counter
//! - Higher ballot always wins
//! - Prevents split-brain after recovery/partition
//!
//! # Example
//!
//! ```ignore
//! let election = LeaderElection::new(subspace);
//!
//! // Initialize (once)
//! db.run(|txn| election.initialize(&txn)).await?;
//!
//! // Register as candidate (once)
//! db.run(|txn| election.register_candidate(&txn, "my-id", 0, now())).await?;
//!
//! // Main loop
//! loop {
//!     let result = db.run(|txn| {
//!         election.run_election_cycle(&txn, "my-id", 0, now())
//!     }).await?;
//!
//!     match result {
//!         ElectionResult::Leader(state) => { /* do leader work */ }
//!         ElectionResult::Follower(_) => { /* follow */ }
//!     }
//!
//!     sleep(config.heartbeat_interval).await;
//! }
//! ```

mod algorithm;
mod errors;
mod keys;
mod types;

pub use errors::{LeaderElectionError, Result};
pub use types::{CandidateInfo, ElectionConfig, ElectionResult, LeaderInfo, LeaderState};

use crate::{tuple::Subspace, Transaction};
use std::ops::Deref;
use std::time::Duration;

/// Main leader election coordinator
///
/// This struct provides the primary interface for participating in leader elections.
/// It encapsulates all election operations within a specific FoundationDB subspace,
/// allowing multiple independent elections to coexist in the same database.
#[derive(Clone)]
pub struct LeaderElection {
    subspace: Subspace,
}

impl LeaderElection {
    /// Create a new leader election instance with the given subspace
    ///
    /// The subspace isolates this election from others in the database.
    /// All election data will be stored under this subspace prefix.
    ///
    /// # Arguments
    /// * `subspace` - The FoundationDB subspace to use for storing election data
    pub fn new(subspace: Subspace) -> Self {
        Self { subspace }
    }

    // ========================================================================
    // INITIALIZATION
    // ========================================================================

    /// Initialize the leader election system with default settings
    ///
    /// This must be called once before any processes can participate in the election.
    /// Sets up the necessary configuration with sensible defaults:
    /// - 10 second lease duration
    /// - 3 second heartbeat interval
    /// - 15 second candidate timeout
    /// - Elections enabled
    /// - Preemption enabled
    ///
    /// This operation is idempotent - calling it multiple times has no effect.
    pub async fn initialize<T>(&self, txn: &T) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::initialize(txn, &self.subspace, ElectionConfig::default()).await
    }

    /// Initialize the leader election system with custom configuration
    ///
    /// Allows fine-tuning election parameters for specific use cases.
    /// This must be called once before any processes can participate.
    ///
    /// # Arguments
    /// * `config` - Custom election configuration
    pub async fn initialize_with_config<T>(&self, txn: &T, config: ElectionConfig) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::initialize(txn, &self.subspace, config).await
    }

    /// Write election configuration
    ///
    /// Updates the global election parameters dynamically.
    ///
    /// # Warning
    /// Changing configuration during active elections may cause temporary
    /// leadership instability.
    pub async fn write_config<T>(&self, txn: &T, config: &ElectionConfig) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::write_config(txn, &self.subspace, config).await
    }

    /// Read current election configuration
    pub async fn read_config<T>(&self, txn: &T) -> Result<ElectionConfig>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::read_config(txn, &self.subspace).await
    }

    // ========================================================================
    // CANDIDATE MANAGEMENT
    // ========================================================================

    /// Register as a candidate
    ///
    /// Registers this process as a candidate for leadership. Uses SetVersionstampedValue
    /// to assign a unique versionstamp at registration time.
    ///
    /// # Arguments
    /// * `process_id` - Unique identifier for this process
    /// * `priority` - Priority level (higher = more preferred for leadership)
    /// * `current_time` - Current time for heartbeat timestamp
    ///
    /// # Note
    /// The versionstamp is assigned once at registration and preserved on heartbeats.
    /// You'll need to read the candidate info after commit to get the actual versionstamp.
    pub async fn register_candidate<T>(
        &self,
        txn: &T,
        process_id: &str,
        priority: i32,
        current_time: Duration,
    ) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::register_candidate(txn, &self.subspace, process_id, priority, current_time).await
    }

    /// Send heartbeat as candidate
    ///
    /// Updates the candidate's timestamp to indicate liveness.
    /// This should be called periodically (at `heartbeat_interval`).
    ///
    /// # Arguments
    /// * `process_id` - Unique identifier for this process
    /// * `priority` - Priority level (can be updated on each heartbeat)
    /// * `current_time` - Current time for heartbeat timestamp
    ///
    /// # Errors
    /// Returns `ProcessNotFound` if not registered
    pub async fn heartbeat_candidate<T>(
        &self,
        txn: &T,
        process_id: &str,
        priority: i32,
        current_time: Duration,
    ) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::heartbeat_candidate(txn, &self.subspace, process_id, priority, current_time)
            .await
    }

    /// Unregister as candidate
    ///
    /// Removes this process from the candidate list.
    /// If leader, call `resign_leadership` first.
    pub async fn unregister_candidate<T>(&self, txn: &T, process_id: &str) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::unregister_candidate(txn, &self.subspace, process_id).await
    }

    /// Get candidate info for a specific process
    pub async fn get_candidate<T>(&self, txn: &T, process_id: &str) -> Result<Option<CandidateInfo>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::get_candidate(txn, &self.subspace, process_id).await
    }

    /// List all alive candidates
    ///
    /// O(N) operation - use sparingly, mainly for monitoring.
    pub async fn list_candidates<T>(
        &self,
        txn: &T,
        current_time: Duration,
    ) -> Result<Vec<CandidateInfo>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::list_candidates(txn, &self.subspace, current_time).await
    }

    /// Remove dead candidates
    ///
    /// O(N) operation - should be called by leader periodically.
    /// Returns count of evicted candidates.
    pub async fn evict_dead_candidates<T>(&self, txn: &T, current_time: Duration) -> Result<usize>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::evict_dead_candidates(txn, &self.subspace, current_time).await
    }

    // ========================================================================
    // LEADERSHIP OPERATIONS (O(1))
    // ========================================================================

    /// Try to claim leadership
    ///
    /// Attempts to become the leader. This is an O(1) operation that:
    /// 1. Looks up candidate registration to get versionstamp
    /// 2. Reads the current leader state
    /// 3. Checks if we can claim (no leader, expired lease, or preemption)
    /// 4. Writes new leader state with incremented ballot
    ///
    /// # Arguments
    /// * `process_id` - Unique identifier for this process
    /// * `priority` - Priority level for preemption decisions
    /// * `current_time` - Current time for lease calculation
    ///
    /// # Returns
    /// * `Ok(Some(state))` - Successfully claimed leadership
    /// * `Ok(None)` - Cannot claim, another valid leader exists
    /// * `Err(UnregisteredCandidate)` - Process is not registered as a candidate
    pub async fn try_claim_leadership<T>(
        &self,
        txn: &T,
        process_id: &str,
        priority: i32,
        current_time: Duration,
    ) -> Result<Option<LeaderState>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::try_claim_leadership(txn, &self.subspace, process_id, priority, current_time)
            .await
    }

    /// Refresh leadership lease
    ///
    /// Called periodically by the leader to extend lease.
    /// Fails if no longer the leader.
    ///
    /// # Returns
    /// * `Ok(Some(state))` - Lease refreshed
    /// * `Ok(None)` - No longer the leader
    pub async fn refresh_lease<T>(
        &self,
        txn: &T,
        process_id: &str,
        current_time: Duration,
    ) -> Result<Option<LeaderState>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::refresh_lease(txn, &self.subspace, process_id, current_time).await
    }

    /// Voluntarily resign leadership
    ///
    /// Immediately releases leadership.
    ///
    /// # Returns
    /// `true` if was leader and resigned, `false` otherwise
    pub async fn resign_leadership<T>(&self, txn: &T, process_id: &str) -> Result<bool>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::resign_leadership(txn, &self.subspace, process_id).await
    }

    /// Check if this process is the current leader
    ///
    /// O(1) operation.
    pub async fn is_leader<T>(
        &self,
        txn: &T,
        process_id: &str,
        current_time: Duration,
    ) -> Result<bool>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::is_leader(txn, &self.subspace, process_id, current_time).await
    }

    /// Get current leader information
    ///
    /// O(1) operation. Returns None if no leader or lease expired.
    pub async fn get_leader<T>(
        &self,
        txn: &T,
        current_time: Duration,
    ) -> Result<Option<LeaderState>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::get_leader(txn, &self.subspace, current_time).await
    }

    /// Get current leader information without lease validation
    ///
    /// O(1) operation. Returns leader state regardless of whether lease has expired.
    /// Useful for debugging, monitoring, and invariant checking.
    pub async fn get_leader_raw<T>(&self, txn: &T) -> Result<Option<LeaderState>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::get_leader_raw(txn, &self.subspace).await
    }

    // ========================================================================
    // HIGH-LEVEL CONVENIENCE API
    // ========================================================================

    /// Run a complete election cycle
    ///
    /// Combines candidate heartbeat + leadership claim in one operation.
    /// This is what most users should call in their main loop.
    ///
    /// # Arguments
    /// * `process_id` - Unique identifier for this process
    /// * `priority` - Priority level
    /// * `current_time` - Current time
    ///
    /// # Returns
    /// `ElectionResult::Leader` if this process is leader, `ElectionResult::Follower` otherwise
    pub async fn run_election_cycle<T>(
        &self,
        txn: &T,
        process_id: &str,
        priority: i32,
        current_time: Duration,
    ) -> Result<ElectionResult>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::run_election_cycle(txn, &self.subspace, process_id, priority, current_time)
            .await
    }
}
