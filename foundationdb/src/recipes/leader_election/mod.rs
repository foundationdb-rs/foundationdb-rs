// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Leader Election module for FoundationDB
//!
//! This module implements a distributed leader election protocol using FoundationDB
//! as shared memory. The algorithm is based on the paper "Leader Election Using NewSQL
//! Database Systems" but differs by directly leveraging FoundationDB's strictly
//! serializable transactions and versionstamps instead of timestamps.
//!
//! # Key Properties
//!
//! - **Integrity**: Guarantees at most one leader at any time through serializable
//!   transactions and atomic operations
//! - **Termination**: A correct process eventually becomes leader if processes keep
//!   sending heartbeats
//! - **Liveness**: Observers can retrieve the current leader ID through read-only
//!   operations without participating in the election
//!
//! # Algorithm Overview
//!
//! The election algorithm works as follows:
//!
//! 1. **Process Registration**: Each process registers with a unique UUID and receives
//!    a versionstamp that captures the transaction's commit version, providing a
//!    time-based ordering using FoundationDB's internal clock
//!
//! 2. **Heartbeat Mechanism**: Processes periodically send heartbeats by updating
//!    their versionstamp. Processes that miss too many heartbeats are considered dead
//!
//! 3. **Leader Selection**: The process with the earliest registration/heartbeat time
//!    (smallest versionstamp) among alive processes becomes the leader
//!
//! 4. **Timeout Management**: Leaders are valid as long as their heartbeat is within the configured
//!    timeout period, preventing split-brain scenarios

mod algorithm;
mod errors;
mod keys;
mod types;

pub use errors::{LeaderElectionError, Result};
pub use types::{ElectionConfig, LeaderInfo, ProcessDescriptor};

use crate::{tuple::Subspace, Transaction};
use std::ops::Deref;

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

    /// Initialize the leader election system with default settings
    ///
    /// This must be called once before any processes can participate in the election.
    /// It sets up the necessary data structures and configuration with sensible defaults:
    /// - Max missed heartbeats: 10M (approximately 10 seconds at normal version rate)
    /// - Election enabled: true
    ///
    /// This operation is idempotent - calling it multiple times has no effect
    /// if the election is already initialized.
    pub async fn initialize<T>(&self, txn: &mut T) -> Result<()>
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
    /// * `config` - Custom election configuration including heartbeat tolerance
    pub async fn initialize_with_config<T>(&self, txn: &mut T, config: ElectionConfig) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::initialize(txn, &self.subspace, config).await
    }

    /// Register a new process in the election
    ///
    /// Registers a process with a unique identifier and assigns it a versionstamp
    /// that captures the transaction's commit version. This versionstamp provides
    /// a time-based ordering - processes that register earlier (lower commit versions)
    /// are preferred for leadership.
    ///
    /// # Arguments
    /// * `process_id` - Unique identifier for the process (e.g., hostname, UUID)
    /// * `timestamp` - Current time to use for timeout detection
    ///
    /// # Errors
    /// Returns `LeaderElectionError::ElectionDisabled` if elections are disabled
    ///
    /// # Note
    /// The process ID should be globally unique. Using duplicate IDs will cause
    /// processes to overwrite each other's heartbeats.
    pub async fn register_process<T>(
        &self,
        txn: &mut T,
        process_id: &str,
        timestamp: std::time::Duration,
    ) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::register_process(txn, &self.subspace, process_id, timestamp).await
    }

    /// Send a heartbeat for the given process
    ///
    /// Updates the process's timestamp to indicate it's still alive.
    /// This should be called periodically (e.g., every 5-10 seconds) to maintain
    /// process liveness.
    ///
    /// # Arguments
    /// * `process_uuid` - The unique identifier used during registration
    /// * `timestamp` - Current time to use for timeout detection
    ///
    /// # Errors
    /// Returns `LeaderElectionError::ElectionDisabled` if elections are disabled
    ///
    /// # Important
    /// Failing to send heartbeats will cause the process to be evicted from
    /// the election after `heartbeat_timeout` expires.
    pub async fn heartbeat<T>(
        &self,
        txn: &mut T,
        process_uuid: &str,
        timestamp: std::time::Duration,
    ) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::heartbeat(txn, &self.subspace, process_uuid, timestamp).await
    }

    /// Try to become the leader
    ///
    /// Attempts to claim leadership if this process was registered/heartbeat earliest
    /// (has the smallest versionstamp) among all alive processes. This operation uses
    /// serializable transactions to ensure at most one leader.
    ///
    /// # Arguments
    /// * `process_uuid` - The unique identifier of the process attempting leadership
    /// * `timestamp` - Current time to use for timeout detection
    ///
    /// # Returns
    /// * `Ok(true)` - Successfully became the leader
    /// * `Ok(false)` - Did not become leader (another process has priority)
    /// * `Err(_)` - Transaction or system error
    ///
    /// # Algorithm
    /// 1. Checks if elections are enabled (creates implicit read conflict)
    /// 2. Finds all alive processes (recent heartbeats)
    /// 3. Checks if this process has the smallest versionstamp
    /// 4. If yes, updates leader state and evicts dead processes
    ///
    /// # Performance
    /// This operation uses serializable transactions, causing automatic
    /// retries if concurrent leadership attempts occur.
    pub async fn try_become_leader<T>(
        &self,
        txn: &mut T,
        process_uuid: &str,
        timestamp: std::time::Duration,
    ) -> Result<bool>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::try_become_leader(txn, &self.subspace, process_uuid, timestamp).await
    }

    /// Get the current leader (read-only, for observers)
    ///
    /// Retrieves information about the current leader without participating
    /// in the election. This is a read-only operation that doesn't cause
    /// conflicts with the election process.
    ///
    /// # Arguments
    /// * `timestamp` - Current time to use for lease validation
    ///
    /// # Returns
    /// * `Some(LeaderInfo)` - Current leader information including lease expiry
    /// * `None` - No current leader (election in progress or disabled)
    ///
    /// # Use Cases
    /// - Observers that need to know the leader without participating
    /// - Load balancers routing requests to the leader
    /// - Monitoring systems tracking leadership changes
    ///
    /// # Note
    /// Leadership is determined by whether the leader's last heartbeat is within
    /// the configured timeout period. Stale leaders are automatically considered invalid.
    pub async fn get_current_leader<T>(
        &self,
        txn: &mut T,
        timestamp: std::time::Duration,
    ) -> Result<Option<LeaderInfo>>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::get_current_leader(txn, &self.subspace, timestamp).await
    }

    /// Check if a specific process is the current leader
    ///
    /// Convenience method to check if a specific process holds leadership.
    /// This combines leader retrieval with identity comparison.
    ///
    /// # Arguments
    /// * `process_uuid` - The process identifier to check
    /// * `timestamp` - Current time to use for timeout detection
    ///
    /// # Returns
    /// * `true` - The specified process is the current leader
    /// * `false` - Process is not leader or election is disabled
    ///
    /// # Note
    /// This is more efficient than calling `get_current_leader()` and comparing
    /// manually as it can short-circuit in some cases.
    pub async fn is_leader<T>(
        &self,
        txn: &mut T,
        process_uuid: &str,
        timestamp: std::time::Duration,
    ) -> Result<bool>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::is_leader(txn, &self.subspace, process_uuid, timestamp).await
    }

    /// Write global election configuration
    ///
    /// Updates the election configuration dynamically. This can be used to:
    /// - Adjust heartbeat tolerance based on network conditions
    /// - Temporarily disable elections for maintenance
    /// - Fine-tune election behavior in production
    ///
    /// # Arguments
    /// * `config` - New configuration to apply
    ///
    /// # Warning
    /// Changing configuration during active elections may cause temporary
    /// leadership instability. Consider disabling elections first, then
    /// re-enabling with new parameters.
    pub async fn write_config<T>(&self, txn: &mut T, config: &ElectionConfig) -> Result<()>
    where
        T: Deref<Target = Transaction>,
    {
        algorithm::write_config(txn, &self.subspace, config).await
    }
}
