// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Core leader election algorithm implementation
//!
//! This module contains the internal implementation of the leader election protocol.
//! It handles process registration, heartbeats, leader selection, and lease management
//! using FoundationDB's transactional guarantees.
//!
//! # Implementation Notes
//!
//! The algorithm leverages several FoundationDB features:
//! - **Versionstamps**: For unique, ordered process identification
//! - **Conflict Ranges**: To ensure exclusive leader transitions
//! - **Atomic Operations**: For consistent versionstamp updates
//! - **Range Reads**: For efficient process discovery
//!
//! # Safety
//!
//! The implementation ensures the safety property (at most one leader) through:
//! - Serializable transactions and atomic versionstamp operations
//! - Lease-based leadership with automatic expiry
//! - Strict ordering of processes by versionstamp

use crate::{
    options,
    tuple::{pack_with_versionstamp, Subspace, Versionstamp},
    RangeOption, RetryableTransaction,
};
use futures::StreamExt;

use super::{
    errors::{LeaderElectionError, Result},
    keys::*,
    types::*,
};

/// Initialize the leader election system
///
/// Sets up the initial configuration for the election. This operation is idempotent -
/// if the system is already initialized, it does nothing.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `config` - Initial configuration parameters
pub async fn initialize(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    config: ElectionConfig,
) -> Result<()> {
    let config_key = config_key(subspace);

    // Check if already initialized
    if txn.get(&config_key, false).await?.is_none() {
        // Pack as tuple: (max_missed_heartbeats, election_enabled)
        let data = (config.max_missed_heartbeats, config.election_enabled);
        let packed = crate::tuple::pack(&data);
        txn.set(&config_key, &packed);
    }

    Ok(())
}

/// Read election configuration from the database
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace containing election data
///
/// # Returns
/// Current election configuration
///
/// # Errors
/// Returns `NotInitialized` if the election system hasn't been initialized
pub async fn read_config(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
) -> Result<ElectionConfig> {
    let config_key = config_key(subspace);
    let data = txn
        .get(&config_key, false)
        .await?
        .ok_or(LeaderElectionError::NotInitialized)?;

    // Unpack tuple: (max_missed_heartbeats, election_enabled)
    let tuple: (u64, bool) = crate::tuple::unpack(&data)?;

    let config = ElectionConfig {
        max_missed_heartbeats: tuple.0,
        election_enabled: tuple.1,
    };
    Ok(config)
}

/// Write election configuration to the database
///
/// Updates the global election parameters. This affects all participating processes.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `config` - New configuration to apply
pub async fn write_config(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    config: &ElectionConfig,
) -> Result<()> {
    let config_key = config_key(subspace);
    let data = (config.max_missed_heartbeats, config.election_enabled);
    let packed = crate::tuple::pack(&data);
    txn.set(&config_key, &packed);
    Ok(())
}

/// Register a new process in the election
///
/// Creates a process entry with a unique versionstamp that captures the transaction's
/// commit version. The versionstamp is assigned by FoundationDB at commit time,
/// providing a time-based ordering for leader selection.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - Unique identifier for the process
///
/// # Errors
/// Returns `ElectionDisabled` if elections are currently disabled
///
/// # Implementation
/// Uses `SetVersionstampedValue` atomic operation to ensure the versionstamp
/// is set at transaction commit time, providing a unique, ordered identifier.
pub async fn register_process(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    process_id: &str,
) -> Result<()> {
    // Check if election is enabled
    let config = read_config(txn, subspace).await?;
    if !config.election_enabled {
        return Err(LeaderElectionError::ElectionDisabled);
    }

    let key = process_key(subspace, process_id);

    // Pack the versionstamp using pack_with_versionstamp
    let packed = pack_with_versionstamp(&Versionstamp::incomplete(0));

    // Use atomic operation to set versionstamped value
    txn.atomic_op(&key, &packed, options::MutationType::SetVersionstampedValue);

    Ok(())
}

/// Send heartbeat for a process
///
/// Updates the process's versionstamp to indicate it's still alive. The new
/// versionstamp reflects the current transaction's commit version.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_uuid` - The process's unique identifier
///
/// # Errors
/// Returns `ElectionDisabled` if elections are currently disabled
///
/// # Note
/// Heartbeats should be sent regularly (e.g., every 1-2 seconds) to prevent
/// the process from being evicted as dead.
pub async fn heartbeat(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    process_uuid: &str,
) -> Result<()> {
    // Check if election is enabled
    let config = read_config(txn, subspace).await?;
    if !config.election_enabled {
        return Err(LeaderElectionError::ElectionDisabled);
    }

    let key = process_key(subspace, process_uuid);

    // Pack the versionstamp using pack_with_versionstamp
    let packed = pack_with_versionstamp(&Versionstamp::incomplete(0));

    txn.atomic_op(&key, &packed, options::MutationType::SetVersionstampedValue);
    Ok(())
}

/// Find all alive processes in the election
///
/// Scans all registered processes and filters out those whose heartbeats are too old.
/// Returns processes sorted by their versionstamp (transaction commit time).
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
///
/// # Returns
/// Vector of (process_uuid, descriptor) pairs sorted by priority
///
/// # Algorithm
/// 1. Read all processes from the processes range
/// 2. Calculate staleness threshold based on max_missed_heartbeats
/// 3. Filter processes whose versionstamp is within the threshold
/// 4. Sort by ProcessDescriptor (ordered by transaction commit version)
///
/// # Performance
/// This operation performs a range read which is efficient in FoundationDB.
/// The number of processes is typically small (< 1000) in production systems.
pub async fn find_alive_processes(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
) -> Result<Vec<(String, ProcessDescriptor)>> {
    let config = read_config(txn, subspace).await?;
    let (start, end) = processes_range(subspace);

    // Get current read version for staleness check
    let current_version = txn.get_read_version().await?;
    let current_version_u64 = current_version as u64;

    let staleness_threshold = config.max_missed_heartbeats * VERSIONSTAMP_PER_HEARTBEAT_ESTIMATE;

    let mut alive_processes = Vec::new();

    // Read all processes
    let range = RangeOption::from((start, end));
    let mut kvs = txn.get_ranges_keyvalues(range, false);

    while let Some(kv) = kvs.next().await {
        let kv = kv?;
        // Extract UUID from key
        let key_tuple: (String, String) = subspace.unpack(kv.key())?;
        let uuid = key_tuple.1;

        // Unpack versionstamp from value
        let versionstamp: Versionstamp = crate::tuple::unpack(kv.value())?;

        let descriptor = ProcessDescriptor::from_versionstamp(*versionstamp.as_bytes());

        // Check if heartbeat is fresh enough
        if version_gap(descriptor.version, current_version_u64) < staleness_threshold {
            alive_processes.push((uuid, descriptor));
        }
    }

    // Sort by version for leader selection (ProcessDescriptor implements Ord)
    alive_processes.sort_by_key(|(_uuid, desc)| desc.clone());

    Ok(alive_processes)
}

/// Try to become the leader
///
/// Core leader election logic. Attempts to claim leadership if this process
/// has the earliest versionstamp (was registered/heartbeat first) among all alive processes.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_uuid` - UUID of the process attempting to become leader
///
/// # Returns
/// * `true` if successfully became leader
/// * `false` if another process has higher priority
///
/// # Algorithm
/// 1. Check if elections are enabled (creates implicit read conflict)
/// 2. Find all alive processes
/// 3. Check if we have the smallest versionstamp (earliest commit time)
/// 4. If yes, update leader state and evict dead processes
///
/// # Safety
/// Serializable transactions ensure only one process can successfully
/// claim leadership in a given transaction. Concurrent attempts will retry.
pub async fn try_become_leader(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    process_uuid: &str,
) -> Result<bool> {
    // Check if election is enabled
    // Note: Reading the config creates an implicit read conflict on the config key,
    // which helps serialize leadership transitions
    let config = read_config(txn, subspace).await?;

    if !config.election_enabled {
        return Ok(false);
    }

    // Find alive processes
    let alive_processes = find_alive_processes(txn, subspace).await?;

    // Check if we're the smallest alive process
    if let Some((smallest_uuid, smallest_desc)) = alive_processes.first() {
        if smallest_uuid == process_uuid {
            // We are the leader!
            update_leader_state(txn, subspace, Some(smallest_desc)).await?;

            // Evict dead processes
            evict_dead_processes(txn, subspace, &alive_processes).await?;

            return Ok(true);
        }
    }

    Ok(false)
}

/// Update the leader state in the database
///
/// Records the new leader's information and sets up their lease.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `leader` - The new leader's descriptor, or None to clear leadership
///
/// # Lease Management
/// The leader is granted a lease for `DEFAULT_LEASE_HEARTBEATS` heartbeat intervals.
/// This prevents other processes from claiming leadership even if the leader
/// temporarily loses connectivity.
async fn update_leader_state(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    leader: Option<&ProcessDescriptor>,
) -> Result<()> {
    let leader_key = leader_state_key(subspace);

    // Just store the leader's versionstamp and lease info
    let data = if let Some(leader_desc) = leader {
        (
            leader_desc.to_versionstamp().to_vec(),
            DEFAULT_LEASE_HEARTBEATS,
            Versionstamp::incomplete(0),
        )
    } else {
        (vec![0u8; 12], 0u64, Versionstamp::incomplete(0))
    };
    let packed = pack_with_versionstamp(&data);

    txn.atomic_op(
        &leader_key,
        &packed,
        options::MutationType::SetVersionstampedValue,
    );

    Ok(())
}

/// Evict dead processes from the election
///
/// Removes process entries that are no longer in the alive set.
/// This cleanup is performed by the leader to maintain a clean process list.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `alive` - List of currently alive processes
///
/// # Note
/// Only the leader performs eviction to avoid conflicts.
/// Dead processes are those that haven't sent heartbeats within
/// the configured threshold.
async fn evict_dead_processes(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    alive: &[(String, ProcessDescriptor)],
) -> Result<()> {
    let alive_uuids: std::collections::HashSet<String> =
        alive.iter().map(|(uuid, _)| uuid.clone()).collect();

    let (start, end) = processes_range(subspace);
    let range = RangeOption::from((start, end));
    let mut all_kvs = txn.get_ranges_keyvalues(range, false);

    while let Some(kv) = all_kvs.next().await {
        let kv = kv?;
        let key_tuple: (String, String) = subspace.unpack(kv.key())?;
        let uuid = &key_tuple.1;
        if !alive_uuids.contains(uuid) {
            txn.clear(kv.key());
        }
    }

    Ok(())
}

/// Get the current leader (read-only operation)
///
/// Retrieves information about the current leader without participating in
/// the election. This operation doesn't add conflict ranges, allowing
/// multiple observers to query leadership without conflicts.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
///
/// # Returns
/// * `Some(LeaderInfo)` if there's an active leader with valid lease
/// * `None` if no leader or lease has expired
///
/// # Lease Validation
/// The function verifies that the leader's lease hasn't expired by comparing
/// the current transaction version with the lease end version.
pub async fn get_current_leader(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
) -> Result<Option<LeaderInfo>> {
    let leader_key = leader_state_key(subspace);

    // Simple read-only access, no conflict ranges
    if let Some(data) = txn.get(&leader_key, false).await? {
        // Unpack tuple: (leader_versionstamp, leader_lease_heartbeats, last_updated)
        let tuple: (Vec<u8>, u64, Versionstamp) = crate::tuple::unpack(&data)?;

        if tuple.1 > 0 {
            // Check if there's an active lease
            let leader_versionstamp: [u8; 12] = tuple.0.try_into().map_err(|_| {
                LeaderElectionError::InvalidState("Invalid versionstamp length".to_string())
            })?;
            let leader_desc = ProcessDescriptor::from_versionstamp(leader_versionstamp);
            let leader_lease_heartbeats = tuple.1;

            // Verify leader lease hasn't expired
            let current_version = txn.get_read_version().await?;
            let current_version_u64 = current_version as u64;

            let lease_duration = leader_lease_heartbeats * VERSIONSTAMP_PER_HEARTBEAT_ESTIMATE;
            let lease_end_version = leader_desc.version + lease_duration;

            if current_version_u64 < lease_end_version {
                return Ok(Some(LeaderInfo {
                    leader: leader_desc,
                    lease_end_version,
                }));
            }
        }
    }

    Ok(None)
}

/// Check if a specific process is the current leader
///
/// Determines if the given process holds leadership by comparing its
/// descriptor with the current leader's descriptor.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_uuid` - The process UUID to check
///
/// # Returns
/// * `true` if the process is the current leader
/// * `false` otherwise
///
/// # Fallback Behavior
/// If no leader is recorded but elections are enabled, checks if the
/// process would be the leader based on current alive processes.
pub async fn is_leader(
    txn: &mut RetryableTransaction,
    subspace: &Subspace,
    process_uuid: &str,
) -> Result<bool> {
    // First check if election is enabled
    let config = read_config(txn, subspace).await?;
    if !config.election_enabled {
        return Ok(false);
    }

    // Get the current leader
    if let Some(leader_info) = get_current_leader(txn, subspace).await? {
        // Get the process descriptor to compare
        let process_key = process_key(subspace, process_uuid);
        if let Some(data) = txn.get(&process_key, false).await? {
            // Unpack versionstamp from process data
            let versionstamp: Versionstamp = crate::tuple::unpack(&data)?;
            let process_desc = ProcessDescriptor::from_versionstamp(*versionstamp.as_bytes());
            return Ok(process_desc == leader_info.leader);
        }
    }

    // If no leader or process not found, check if we would be leader
    let alive_processes = find_alive_processes(txn, subspace).await?;
    if let Some((smallest_uuid, _)) = alive_processes.first() {
        return Ok(smallest_uuid == process_uuid);
    }

    Ok(false)
}
