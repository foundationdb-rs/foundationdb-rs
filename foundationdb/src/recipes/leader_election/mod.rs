// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! # Leader Election for FoundationDB
//!
//! A distributed leader election recipe using FoundationDB as coordination backend.
//! Similar to [Apache Curator's LeaderLatch](https://curator.apache.org/) for ZooKeeper,
//! but leveraging FDB's serializable transactions for stronger guarantees.
//!
//! ## When to Use This
//!
//! **Good use cases:**
//! - Singleton services (only one instance should be active)
//! - Job schedulers (one coordinator assigns work)
//! - Primary/backup failover
//! - Exclusive access to external resources
//!
//! **Consider alternatives if:**
//! - You need mutex/lock semantics for short critical sections
//!   (use FoundationDB transactions directly)
//! - You need fair queuing (this uses priority-based preemption)
//!
//! ## API Overview
//!
//! The main entry point is [`LeaderElection`](leader_election::LeaderElection). Typical usage follows this pattern:
//!
//! | Step | Method | Frequency |
//! |------|--------|-----------|
//! | 1. Setup | [`new`](leader_election::LeaderElection::new) | Once per process |
//! | 2. Initialize | [`initialize`](leader_election::LeaderElection::initialize) | Once globally (idempotent) |
//! | 3. Register | [`register_candidate`](leader_election::LeaderElection::register_candidate) | Once per process |
//! | 4. Election loop | [`run_election_cycle`](leader_election::LeaderElection::run_election_cycle) | Every heartbeat interval |
//! | 5. Shutdown | [`resign_leadership`](leader_election::LeaderElection::resign_leadership) + [`unregister_candidate`](leader_election::LeaderElection::unregister_candidate) | On graceful exit |
//!
//! For advanced use cases, lower-level methods are available:
//! - [`try_claim_leadership`](leader_election::LeaderElection::try_claim_leadership) - Attempt to become leader
//! - [`refresh_lease`](leader_election::LeaderElection::refresh_lease) - Extend leadership lease
//! - [`get_leader`](leader_election::LeaderElection::get_leader) - Query current leader
//! - [`is_leader`](leader_election::LeaderElection::is_leader) - Check if this process is leader
//!
//! ## Key Concepts
//!
//! ### Ballots
//! Ballot numbers work like Raft's term - a monotonically increasing counter
//! that establishes ordering. Higher ballot always wins. Each leadership claim
//! or lease refresh increments the ballot. This prevents split-brain scenarios
//! after network partitions heal.
//!
//! The ballot is returned in [`LeaderState::ballot`](leader_election::LeaderState::ballot) and can be used as a
//! fencing token when accessing external resources.
//!
//! ### Leases
//! Leaders hold time-bounded leases configured via [`lease_duration`](leader_election::ElectionConfig::lease_duration).
//! A leader must call [`run_election_cycle`](leader_election::LeaderElection::run_election_cycle) (or [`refresh_lease`](leader_election::LeaderElection::refresh_lease))
//! before the lease expires to maintain leadership.
//!
//! If a leader fails to refresh (crash, network partition), other candidates
//! can claim leadership after the lease expires.
//!
//! ### Preemption
//! When [`allow_preemption`](leader_election::ElectionConfig::allow_preemption) is true, higher-priority candidates
//! can preempt lower-priority leaders. Priority is set via the `priority` parameter
//! in [`register_candidate`](leader_election::LeaderElection::register_candidate). This enables graceful leadership migration
//! to new machines during rolling deployments or infrastructure upgrades.
//!
//! ## Configuration
//!
//! Configure via [`ElectionConfig`](leader_election::ElectionConfig) passed to [`initialize_with_config`](leader_election::LeaderElection::initialize_with_config):
//!
//! | Field | Default | Description |
//! |-------|---------|-------------|
//! | [`lease_duration`](leader_election::ElectionConfig::lease_duration) | 10s | How long leadership is valid without refresh |
//! | [`heartbeat_interval`](leader_election::ElectionConfig::heartbeat_interval) | 3s | Recommended interval for calling `run_election_cycle` |
//! | [`candidate_timeout`](leader_election::ElectionConfig::candidate_timeout) | 15s | When to consider candidates dead |
//! | [`election_enabled`](leader_election::ElectionConfig::election_enabled) | true | Enable/disable elections globally |
//! | [`allow_preemption`](leader_election::ElectionConfig::allow_preemption) | true | Allow priority-based preemption |
//!
//! **Rule of thumb:** `heartbeat_interval` should be less than `lease_duration / 3`
//! to allow retries before lease expires.
//!
//! ## Return Types
//!
//! - [`ElectionResult`](leader_election::ElectionResult) - Returned by [`run_election_cycle`](leader_election::LeaderElection::run_election_cycle), indicates
//!   whether this process is the leader or a follower
//! - [`LeaderState`](leader_election::LeaderState) - Information about a leader: process ID, ballot, lease expiry
//! - [`CandidateInfo`](leader_election::CandidateInfo) - Information about a registered candidate
//!
//! ## Safety Properties
//!
//! - **Mutual Exclusion**: At most one leader at any time (guaranteed by FDB serializable transactions)
//! - **Liveness**: A correct process eventually becomes leader
//! - **Consistency**: Ballot numbers provide total ordering of leadership changes
//!
//! ## Simulation Testing
//!
//! This implementation is validated through FoundationDB's deterministic simulation
//! framework under extreme conditions including network partitions, process failures,
//! and clock skew up to ±2 seconds.
//!
//! Key invariants verified:
//! - No overlapping leadership (mutual exclusion)
//! - Ballot monotonicity (ballots never regress)
//! - Fencing token validity (each claim increments ballot)
//!
//! See `foundationdb-recipes-simulation` crate for test configurations.

mod algorithm;
mod errors;
mod keys;
mod types;

pub use errors::{LeaderElectionError, Result};
pub use types::{CandidateInfo, ElectionConfig, ElectionResult, LeaderState};

use crate::{tuple::Subspace, Transaction};
use std::ops::Deref;
use std::time::Duration;

/// Coordinator for distributed leader election.
///
/// `LeaderElection` provides the interface for participating in leader elections
/// backed by FoundationDB. Multiple independent elections can coexist by using
/// different [`Subspace`]s.
///
/// # Lifecycle
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────────┐
/// │  1. new()              Create instance with subspace            │
/// │  2. initialize()       Set up election (once globally)          │
/// │  3. register_candidate() Join as candidate (once per process)   │
/// │  4. run_election_cycle()  Main loop (every heartbeat_interval)  │
/// │  5. resign_leadership() + unregister_candidate()  Cleanup       │
/// └─────────────────────────────────────────────────────────────────┘
/// ```
///
/// # Methods by Category
///
/// ## Setup
/// - [`new`](Self::new) - Create election instance
/// - [`initialize`](Self::initialize) - Initialize with defaults
/// - [`initialize_with_config`](Self::initialize_with_config) - Initialize with custom [`ElectionConfig`]
///
/// ## Candidate Management
/// - [`register_candidate`](Self::register_candidate) - Join the election
/// - [`heartbeat_candidate`](Self::heartbeat_candidate) - Send liveness heartbeat
/// - [`unregister_candidate`](Self::unregister_candidate) - Leave the election
/// - [`get_candidate`](Self::get_candidate) - Query candidate info
/// - [`list_candidates`](Self::list_candidates) - List all alive candidates (O(N))
/// - [`evict_dead_candidates`](Self::evict_dead_candidates) - Remove timed-out candidates (O(N))
///
/// ## Leadership Operations (O(1))
/// - [`try_claim_leadership`](Self::try_claim_leadership) - Attempt to become leader
/// - [`refresh_lease`](Self::refresh_lease) - Extend leadership lease
/// - [`resign_leadership`](Self::resign_leadership) - Voluntarily step down
/// - [`is_leader`](Self::is_leader) - Check if this process is leader
/// - [`get_leader`](Self::get_leader) - Get current leader (lease-validated)
/// - [`get_leader_raw`](Self::get_leader_raw) - Get current leader (no lease check)
///
/// ## High-Level API
/// - [`run_election_cycle`](Self::run_election_cycle) - Combined heartbeat + claim (recommended for main loop)
///
/// ## Configuration
/// - [`read_config`](Self::read_config) - Read current configuration
/// - [`write_config`](Self::write_config) - Update configuration dynamically
///
/// # Thread Safety
///
/// `LeaderElection` is [`Clone`], [`Send`], and [`Sync`]. It holds only a
/// [`Subspace`] and can be safely shared across tasks.
/// Each method operates within the provided transaction's scope.
///
/// # Related Types
///
/// - [`ElectionConfig`] - Configuration parameters
/// - [`ElectionResult`] - Return type from [`run_election_cycle`](Self::run_election_cycle)
/// - [`LeaderState`] - Leader information including ballot (fencing token)
/// - [`CandidateInfo`] - Registered candidate information
/// - [`LeaderElectionError`] - Error types for this module
///
/// # See Also
///
/// See the [module documentation](self) for algorithm details, key concepts
/// (ballots, leases, preemption), and configuration guidelines.
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
        algorithm::run_election_cycle(txn, &self.subspace, process_id, priority, current_time).await
    }
}
