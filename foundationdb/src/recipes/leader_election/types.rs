// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Core data structures for leader election
//!
//! This module defines the fundamental types used in the leader election algorithm,
//! including process descriptors, configuration, and leader information.

/// Process descriptor with versionstamp-based ID
///
/// Represents a process participating in leader election. Each process is uniquely
/// identified by its versionstamp, which captures the transaction commit version.
///
/// The descriptor uses FoundationDB's versionstamp feature to ensure:
/// - Globally unique identification based on transaction commit time
/// - Total ordering using FoundationDB's internal clock for leader selection  
/// - Automatic timestamp for liveness checking
///
/// # Ordering
///
/// Processes are ordered by their transaction commit version, providing a
/// time-based ordering using FoundationDB's internal clock. The process that
/// registered or sent a heartbeat earliest (smallest versionstamp) becomes leader.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessDescriptor {
    /// The transaction version from the versionstamp (first 8 bytes as u64)
    ///
    /// This is the global commit version assigned by FoundationDB when the
    /// process registered or last sent a heartbeat. It represents a point in
    /// FoundationDB's logical time, with lower values indicating earlier transactions.
    pub version: u64,
    /// The user version from the versionstamp (last 2 bytes as u16)
    ///
    /// Can be used to differentiate between processes that register in the
    /// same transaction. Typically 0 for normal operations.
    pub user_version: u16,
}

impl ProcessDescriptor {
    /// Create a ProcessDescriptor from a raw 12-byte versionstamp
    ///
    /// # Arguments
    /// * `versionstamp` - Raw versionstamp bytes from FoundationDB
    ///
    /// # Format
    /// - Bytes 0-7: Transaction version (big-endian u64)
    /// - Bytes 8-9: Batch number (ignored, used for ordering within transaction)
    /// - Bytes 10-11: User version (big-endian u16)
    pub fn from_versionstamp(versionstamp: [u8; 12]) -> Self {
        let version = u64::from_be_bytes(versionstamp[0..8].try_into().unwrap());
        let user_version = u16::from_be_bytes([versionstamp[10], versionstamp[11]]);
        Self {
            version,
            user_version,
        }
    }

    /// Convert the ProcessDescriptor back to a raw versionstamp
    ///
    /// # Returns
    /// 12-byte array suitable for storage in FoundationDB
    ///
    /// # Note
    /// Bytes 8-9 (batch number) are set to 0 as they're not preserved
    pub fn to_versionstamp(&self) -> [u8; 12] {
        let mut vs = [0u8; 12];
        vs[0..8].copy_from_slice(&self.version.to_be_bytes());
        vs[10..12].copy_from_slice(&self.user_version.to_be_bytes());
        vs
    }
}

/// Global variables for leader election configuration
///
/// Controls the behavior of the election system. These parameters can be
/// adjusted at runtime to tune election sensitivity and performance.
#[derive(Debug, Clone)]
pub struct ElectionConfig {
    /// Maximum number of missed heartbeats before a process is considered dead
    ///
    /// This value determines how many version increments a process can miss
    /// before being evicted from the election.
    ///
    /// # Tuning Guidelines
    /// - Lower values: Faster failure detection but more sensitive to network hiccups
    /// - Higher values: More tolerance for temporary issues but slower failover
    /// - Default: 10M versions (~10 seconds at normal commit rate)
    pub max_missed_heartbeats: u64,
    /// Whether election is currently enabled
    ///
    /// When disabled:
    /// - No new leaders can be elected
    /// - Existing leader remains until lease expires
    /// - Processes can still register and send heartbeats
    ///
    /// Useful for maintenance windows or graceful shutdown scenarios
    pub election_enabled: bool,
}

impl Default for ElectionConfig {
    /// Creates default configuration with sensible production values
    ///
    /// - 10 second heartbeat tolerance (10M versions)
    /// - Elections enabled
    fn default() -> Self {
        Self {
            max_missed_heartbeats: DEFAULT_LEASE_HEARTBEATS * VERSIONSTAMP_PER_HEARTBEAT_ESTIMATE,
            election_enabled: true,
        }
    }
}

/// Information about the current leader
///
/// Contains both the leader's identity and lease information.
/// The lease prevents split-brain scenarios during network partitions.
#[derive(Debug, Clone)]
pub struct LeaderInfo {
    /// The leader's process descriptor
    ///
    /// Contains the versionstamp identifying the leading process.
    /// Can be compared with local process descriptors to check leadership.
    pub leader: ProcessDescriptor,
    /// When the leader's lease expires (version number)
    ///
    /// The lease expires at this FoundationDB version. After expiry,
    /// a new leader can be elected even if the old leader is still alive.
    ///
    /// This prevents permanent leadership loss if the leader becomes
    /// isolated but doesn't crash.
    pub lease_end_version: u64,
}

/// Default lease duration in heartbeats
///
/// Number of heartbeat intervals a leader's lease remains valid.
/// After this many missed heartbeats, leadership can be transferred
/// even if the leader process hasn't been evicted yet.
pub const DEFAULT_LEASE_HEARTBEATS: u64 = 10;

/// Estimated versionstamps per heartbeat interval
///
/// FoundationDB increments versions at approximately 1M/second under normal load.
/// This can spike higher during recovery or under heavy load.
///
/// Used to convert heartbeat counts to version numbers for staleness checks.
/// The estimate doesn't need to be precise - it's used for approximate timing.
pub const VERSIONSTAMP_PER_HEARTBEAT_ESTIMATE: u64 = 1_000_000;

/// Calculate the gap between two version numbers
///
/// Safely computes the difference between version numbers, handling cases
/// where the end version might be before the start (e.g., clock skew).
///
/// # Arguments
/// * `start` - Earlier version number
/// * `end` - Later version number
///
/// # Returns
/// The difference if end >= start, otherwise 0
pub fn version_gap(start: u64, end: u64) -> u64 {
    end.saturating_sub(start)
}
