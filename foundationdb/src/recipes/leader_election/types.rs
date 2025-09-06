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

use std::time::Duration;

/// Process descriptor with hybrid versionstamp and Duration-based tracking
///
/// Represents a process participating in leader election. Uses a hybrid approach:
/// - Versionstamp for process ordering (immune to FDB recovery version jumps)
/// - Duration timestamp for timeout detection (survives FDB recovery)
///
/// # Key Design
///
/// The descriptor separates concerns:
/// - **Ordering**: Uses versionstamp which provides total ordering based on registration time
/// - **Timeout Detection**: Uses Duration since epoch for robust timeout calculations
///
/// This design survives FoundationDB cluster recovery where version numbers can
/// jump by millions, breaking version-based timeout detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessDescriptor {
    /// The transaction version from the versionstamp (first 8 bytes as u64)
    ///
    /// Used ONLY for process ordering. The process with the smallest versionstamp
    /// (earliest registration) becomes the leader.
    pub version: u64,
    /// The user version from the versionstamp (last 2 bytes as u16)
    ///
    /// Can be used to differentiate between processes that register in the
    /// same transaction. Typically 0 for normal operations.
    pub user_version: u16,
    /// Duration since Unix epoch when this descriptor was created
    ///
    /// Used for timeout detection. This is immune to FoundationDB version jumps
    /// during cluster recovery and provides accurate timeout calculations.
    pub timestamp: Duration,
}

/// Manual implementation of Ord to ensure ordering by versionstamp only
/// The timestamp field is NOT used for ordering to maintain consistency
impl Ord for ProcessDescriptor {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.version
            .cmp(&other.version)
            .then_with(|| self.user_version.cmp(&other.user_version))
    }
}

impl PartialOrd for ProcessDescriptor {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl ProcessDescriptor {
    /// Create a ProcessDescriptor from versionstamp and explicit timestamp
    ///
    /// # Arguments
    /// * `versionstamp` - Raw versionstamp bytes from FoundationDB
    /// * `timestamp` - Duration since Unix epoch
    pub fn from_versionstamp_and_timestamp(versionstamp: [u8; 12], timestamp: Duration) -> Self {
        let version = u64::from_be_bytes(versionstamp[0..8].try_into().unwrap());
        let user_version = u16::from_be_bytes([versionstamp[10], versionstamp[11]]);
        Self {
            version,
            user_version,
            timestamp,
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

    /// Check if this process descriptor is considered alive
    ///
    /// # Arguments
    /// * `current_time` - Current system time
    /// * `timeout` - Maximum age before considering the process dead
    ///
    /// # Returns
    /// true if the process is still considered alive
    pub fn is_alive(&self, current_time: Duration, timeout: Duration) -> bool {
        current_time.saturating_sub(self.timestamp) <= timeout
    }
}

/// Global variables for leader election configuration
///
/// Controls the behavior of the election system. These parameters can be
/// adjusted at runtime to tune election sensitivity and performance.
#[derive(Debug, Clone)]
pub struct ElectionConfig {
    /// Maximum age before a process is considered dead
    ///
    /// This Duration-based timeout is immune to FoundationDB version jumps
    /// during cluster recovery, providing reliable failure detection.
    ///
    /// # Tuning Guidelines
    /// - Lower values: Faster failure detection but more sensitive to network hiccups
    /// - Higher values: More tolerance for temporary issues but slower failover
    /// - Default: 10 seconds (suitable for most production environments)
    pub heartbeat_timeout: Duration,
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
    /// - 10 second heartbeat timeout
    /// - Elections enabled
    fn default() -> Self {
        Self {
            heartbeat_timeout: DEFAULT_HEARTBEAT_TIMEOUT,
            election_enabled: true,
        }
    }
}

/// Information about the current leader
///
/// Contains the leader's identity. Leadership is determined by
/// whether the leader's last heartbeat is within the timeout period.
#[derive(Debug, Clone)]
pub struct LeaderInfo {
    /// The leader's process descriptor
    ///
    /// Contains the versionstamp identifying the leading process.
    /// Can be compared with local process descriptors to check leadership.
    pub leader: ProcessDescriptor,
}

/// Default heartbeat timeout duration
///
/// Maximum age before a process is considered dead.
/// This Duration-based timeout is immune to FoundationDB version jumps
/// during cluster recovery, providing reliable failure detection.
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
