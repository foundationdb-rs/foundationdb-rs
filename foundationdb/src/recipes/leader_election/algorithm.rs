// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Ballot-based Leader Election Algorithm
//!
//! This module implements an O(1) leader election protocol using a ballot-based
//! approach similar to Raft's term concept.
//!
//! # Design Overview
//!
//! Instead of scanning all candidates to find the leader (O(N)), we store the
//! leader state explicitly at a single key with a ballot number.
//!
//! ## Ballot Numbers
//!
//! Ballot numbers work like Raft's term:
//! - Monotonically increasing counter
//! - Higher ballot always wins
//! - Prevents split-brain after recovery/partition
//! - Incremented on every leadership claim or refresh
//!
//! ## Key Operations (all O(1))
//!
//! - `try_claim_leadership`: Read leader key, check if can claim, write new state
//! - `refresh_lease`: Read leader key, verify identity, write with new ballot
//! - `get_leader`: Single key read
//! - `is_leader`: Single key read + comparison
//!
//! ## Safety
//!
//! FoundationDB's serializable transactions ensure safety:
//! - Reading leader key creates a read conflict
//! - Writing leader key creates a write conflict
//! - Concurrent claims result in conflict, one wins, others retry
//! - On retry, losing processes see updated state
//!
//! # Candidate Management
//!
//! Candidates are stored separately from leadership:
//! - `register_candidate`: Uses SetVersionstampedValue for ordering (once)
//! - `heartbeat_candidate`: Regular set, preserves versionstamp
//! - Candidates exist independently of leadership

use crate::{
    options::MutationType,
    tuple::{pack, pack_with_versionstamp, unpack, Subspace, Versionstamp},
    RangeOption, Transaction,
};
use futures::StreamExt;
use std::ops::Deref;
use std::time::Duration;

use super::{
    errors::{LeaderElectionError, Result},
    keys::*,
    types::*,
};

// ============================================================================
// CONFIGURATION OPERATIONS
// ============================================================================

/// Initialize the leader election system with configuration
///
/// Sets up the initial configuration for the election. This operation is idempotent -
/// if the system is already initialized, it does nothing.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `config` - Initial configuration parameters
pub async fn initialize<T>(txn: &T, subspace: &Subspace, config: ElectionConfig) -> Result<()>
where
    T: Deref<Target = Transaction>,
{
    let key = config_key(subspace);

    // Check if already initialized
    if txn.get(&key, false).await?.is_none() {
        write_config_internal(txn, &key, &config);
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
pub async fn read_config<T>(txn: &T, subspace: &Subspace) -> Result<ElectionConfig>
where
    T: Deref<Target = Transaction>,
{
    let key = config_key(subspace);
    let data = txn
        .get(&key, false)
        .await?
        .ok_or(LeaderElectionError::NotInitialized)?;

    // Unpack tuple: (lease_duration_nanos, heartbeat_interval_nanos, candidate_timeout_nanos, election_enabled, allow_preemption)
    let tuple: (u64, u64, u64, bool, bool) = unpack(&data)?;

    Ok(ElectionConfig {
        lease_duration: Duration::from_nanos(tuple.0),
        heartbeat_interval: Duration::from_nanos(tuple.1),
        candidate_timeout: Duration::from_nanos(tuple.2),
        election_enabled: tuple.3,
        allow_preemption: tuple.4,
    })
}

/// Write election configuration to the database
///
/// Updates the global election parameters. This affects all participating processes.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `config` - New configuration to apply
pub async fn write_config<T>(txn: &T, subspace: &Subspace, config: &ElectionConfig) -> Result<()>
where
    T: Deref<Target = Transaction>,
{
    let key = config_key(subspace);
    write_config_internal(txn, &key, config);
    Ok(())
}

fn write_config_internal<T>(txn: &T, key: &[u8], config: &ElectionConfig)
where
    T: Deref<Target = Transaction>,
{
    let data = (
        config.lease_duration.as_nanos() as u64,
        config.heartbeat_interval.as_nanos() as u64,
        config.candidate_timeout.as_nanos() as u64,
        config.election_enabled,
        config.allow_preemption,
    );
    let packed = pack(&data);
    txn.set(key, &packed);
}

// ============================================================================
// LEADER STATE HELPERS
// ============================================================================

/// Read leader state from the leader key
async fn read_leader_state<T>(txn: &T, key: &[u8]) -> Result<Option<LeaderState>>
where
    T: Deref<Target = Transaction>,
{
    let data = match txn.get(key, false).await? {
        Some(d) => d,
        None => return Ok(None),
    };

    // Unpack tuple: (ballot, leader_id, priority, lease_expiry_nanos, versionstamp_bytes)
    let tuple: (u64, String, i32, u64, Vec<u8>) = unpack(&data)?;

    let versionstamp: [u8; 12] = tuple.4.try_into().map_err(|_| {
        LeaderElectionError::InvalidState("Invalid versionstamp length".to_string())
    })?;

    Ok(Some(LeaderState {
        ballot: tuple.0,
        leader_id: tuple.1,
        priority: tuple.2,
        lease_expiry_nanos: tuple.3,
        versionstamp,
    }))
}

/// Write leader state to the leader key
fn write_leader_state<T>(txn: &T, key: &[u8], state: &LeaderState)
where
    T: Deref<Target = Transaction>,
{
    let data = (
        state.ballot,
        state.leader_id.clone(),
        state.priority,
        state.lease_expiry_nanos,
        state.versionstamp.to_vec(),
    );
    let packed = pack(&data);
    txn.set(key, &packed);
}

// ============================================================================
// LEADER OPERATIONS - ALL O(1)
// ============================================================================

/// Try to claim leadership
///
/// This is the core leader election operation:
/// 1. Look up candidate registration (O(1))
/// 2. Read current leader state (O(1))
/// 3. Decide if we can claim based on ballot/priority/lease
/// 4. Write new state with incremented ballot (O(1))
///
/// FDB transaction conflict detection ensures safety.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - ID of the process attempting to claim leadership
/// * `my_priority` - This process's priority (higher = more preferred)
/// * `current_time` - Current time for lease calculation
///
/// # Returns
/// * `Ok(Some(state))` - Successfully claimed leadership
/// * `Ok(None)` - Cannot claim, another valid leader exists
/// * `Err(UnregisteredCandidate)` - Process is not registered as a candidate
/// * `Err(_)` - Other error occurred
pub async fn try_claim_leadership<T>(
    txn: &T,
    subspace: &Subspace,
    process_id: &str,
    my_priority: i32,
    current_time: Duration,
) -> Result<Option<LeaderState>>
where
    T: Deref<Target = Transaction>,
{
    let config = read_config(txn, subspace).await?;
    if !config.election_enabled {
        return Err(LeaderElectionError::ElectionDisabled);
    }

    // Look up candidate to get versionstamp - only registered candidates can claim leadership
    let candidate = get_candidate(txn, subspace, process_id)
        .await?
        .ok_or(LeaderElectionError::UnregisteredCandidate)?;
    let my_versionstamp = candidate.versionstamp;

    let key = leader_key(subspace);
    let current_leader = read_leader_state(txn, &key).await?;

    let can_claim = match &current_leader {
        None => true, // No leader exists
        Some(leader) => {
            leader.leader_id == process_id // Already leader (refresh)
                || !leader.is_lease_valid(current_time) // Lease expired
                || (config.allow_preemption && my_priority > leader.priority) // Preemption
        }
    };

    if !can_claim {
        return Ok(None);
    }

    // Claim leadership with incremented ballot
    let new_ballot = current_leader.as_ref().map(|l| l.ballot + 1).unwrap_or(1);
    let lease_expiry = current_time + config.lease_duration;

    let new_state = LeaderState {
        ballot: new_ballot,
        leader_id: process_id.to_string(),
        priority: my_priority,
        lease_expiry_nanos: lease_expiry.as_nanos() as u64,
        versionstamp: my_versionstamp,
    };

    write_leader_state(txn, &key, &new_state);
    Ok(Some(new_state))
}

/// Refresh leadership lease
///
/// Called periodically by the leader to extend lease.
/// Fails if no longer the leader (preempted or lease expired).
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - ID of the process refreshing its lease
/// * `current_time` - Current time for lease calculation
///
/// # Returns
/// * `Ok(Some(state))` - Lease refreshed successfully
/// * `Ok(None)` - No longer the leader
pub async fn refresh_lease<T>(
    txn: &T,
    subspace: &Subspace,
    process_id: &str,
    current_time: Duration,
) -> Result<Option<LeaderState>>
where
    T: Deref<Target = Transaction>,
{
    let config = read_config(txn, subspace).await?;
    let key = leader_key(subspace);

    let current = read_leader_state(txn, &key).await?;

    match current {
        Some(leader) if leader.leader_id == process_id => {
            // Still leader - refresh with incremented ballot
            let new_state = LeaderState {
                ballot: leader.ballot + 1,
                lease_expiry_nanos: (current_time + config.lease_duration).as_nanos() as u64,
                leader_id: leader.leader_id,
                priority: leader.priority,
                versionstamp: leader.versionstamp,
            };
            write_leader_state(txn, &key, &new_state);
            Ok(Some(new_state))
        }
        _ => Ok(None), // Not leader anymore
    }
}

/// Voluntarily resign leadership
///
/// Immediately releases leadership. Other candidates can claim.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - ID of the process resigning
///
/// # Returns
/// `true` if was leader and resigned, `false` otherwise
pub async fn resign_leadership<T>(txn: &T, subspace: &Subspace, process_id: &str) -> Result<bool>
where
    T: Deref<Target = Transaction>,
{
    let key = leader_key(subspace);
    let current = read_leader_state(txn, &key).await?;

    match current {
        Some(leader) if leader.leader_id == process_id => {
            txn.clear(&key);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Check if this process is the current leader
///
/// Fast read-only check (O(1)).
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - ID of the process to check
/// * `current_time` - Current time for lease validation
///
/// # Returns
/// `true` if the process is the current leader with valid lease
pub async fn is_leader<T>(
    txn: &T,
    subspace: &Subspace,
    process_id: &str,
    current_time: Duration,
) -> Result<bool>
where
    T: Deref<Target = Transaction>,
{
    let config = read_config(txn, subspace).await?;
    if !config.election_enabled {
        return Err(LeaderElectionError::ElectionDisabled);
    }

    match get_leader(txn, subspace, current_time).await? {
        Some(leader) => Ok(leader.leader_id == process_id),
        None => Ok(false),
    }
}

/// Get current leader information
///
/// Returns None if no leader or lease expired.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `current_time` - Current time for lease validation
///
/// # Returns
/// Current leader state if one exists with valid lease
pub async fn get_leader<T>(
    txn: &T,
    subspace: &Subspace,
    current_time: Duration,
) -> Result<Option<LeaderState>>
where
    T: Deref<Target = Transaction>,
{
    let key = leader_key(subspace);
    let leader = read_leader_state(txn, &key).await?;

    match leader {
        Some(l) if l.is_lease_valid(current_time) => Ok(Some(l)),
        _ => Ok(None),
    }
}

/// Get current leader information without lease validation
///
/// Returns the leader state regardless of whether the lease has expired.
/// Useful for debugging and invariant checking.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
///
/// # Returns
/// Current leader state if one exists, regardless of lease validity
pub async fn get_leader_raw<T>(txn: &T, subspace: &Subspace) -> Result<Option<LeaderState>>
where
    T: Deref<Target = Transaction>,
{
    let key = leader_key(subspace);
    read_leader_state(txn, &key).await
}

// ============================================================================
// CANDIDATE MANAGEMENT
// ============================================================================

/// Register as a candidate
///
/// This is separate from leadership. A process must be a candidate
/// to claim leadership, but being a candidate doesn't make you leader.
///
/// Uses SetVersionstampedValue for registration ordering. The versionstamp
/// is assigned by FDB at commit time and never changes on heartbeat.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - Unique identifier for this process
/// * `priority` - This process's priority (higher = more preferred)
/// * `current_time` - Current time for heartbeat timestamp
pub async fn register_candidate<T>(
    txn: &T,
    subspace: &Subspace,
    process_id: &str,
    priority: i32,
    current_time: Duration,
) -> Result<()>
where
    T: Deref<Target = Transaction>,
{
    let config = read_config(txn, subspace).await?;
    if !config.election_enabled {
        return Err(LeaderElectionError::ElectionDisabled);
    }

    let key = candidate_key(subspace, process_id);

    // Versionstamp is assigned by FDB at commit - ONLY done once at registration
    // Tuple: (priority, timestamp_nanos, versionstamp)
    let data = (
        priority,
        current_time.as_nanos() as u64,
        Versionstamp::incomplete(0),
    );
    let packed = pack_with_versionstamp(&data);
    txn.atomic_op(&key, &packed, MutationType::SetVersionstampedValue);

    Ok(())
}

/// Send heartbeat as candidate
///
/// Updates last-seen timestamp while preserving the versionstamp.
/// This uses a regular set operation (not atomic), which:
/// 1. Fixes error 2000 (can read after write in same transaction)
/// 2. Preserves the original versionstamp for ordering
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - Unique identifier for this process
/// * `priority` - This process's priority (can be updated)
/// * `current_time` - Current time for heartbeat timestamp
///
/// # Errors
/// Returns `ProcessNotFound` if not registered
pub async fn heartbeat_candidate<T>(
    txn: &T,
    subspace: &Subspace,
    process_id: &str,
    priority: i32,
    current_time: Duration,
) -> Result<()>
where
    T: Deref<Target = Transaction>,
{
    let config = read_config(txn, subspace).await?;
    if !config.election_enabled {
        return Err(LeaderElectionError::ElectionDisabled);
    }

    let key = candidate_key(subspace, process_id);

    // READ existing entry to get versionstamp
    let existing = txn
        .get(&key, false)
        .await?
        .ok_or_else(|| LeaderElectionError::ProcessNotFound(process_id.to_string()))?;

    // Unpack: (priority, timestamp_nanos, versionstamp)
    let tuple: (i32, u64, Versionstamp) = unpack(&existing)?;
    let versionstamp = tuple.2;

    // WRITE with regular set, preserving versionstamp
    let data = (priority, current_time.as_nanos() as u64, versionstamp);
    let packed = pack(&data); // Regular pack, NOT pack_with_versionstamp
    txn.set(&key, &packed); // Regular set, NOT atomic_op

    Ok(())
}

/// Unregister as candidate
///
/// Removes candidate registration. If this process was leader,
/// it should call resign_leadership first.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - Unique identifier for this process
pub async fn unregister_candidate<T>(txn: &T, subspace: &Subspace, process_id: &str) -> Result<()>
where
    T: Deref<Target = Transaction>,
{
    let key = candidate_key(subspace, process_id);
    txn.clear(&key);
    Ok(())
}

/// Get candidate info for a specific process
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - Unique identifier for the process
///
/// # Returns
/// Candidate info if registered, None otherwise
pub async fn get_candidate<T>(
    txn: &T,
    subspace: &Subspace,
    process_id: &str,
) -> Result<Option<CandidateInfo>>
where
    T: Deref<Target = Transaction>,
{
    let key = candidate_key(subspace, process_id);
    let data = match txn.get(&key, false).await? {
        Some(d) => d,
        None => return Ok(None),
    };

    // Unpack: (priority, timestamp_nanos, versionstamp)
    let tuple: (i32, u64, Versionstamp) = unpack(&data)?;

    Ok(Some(CandidateInfo {
        process_id: process_id.to_string(),
        priority: tuple.0,
        last_heartbeat_nanos: tuple.1,
        versionstamp: *tuple.2.as_bytes(),
    }))
}

/// List all registered candidates
///
/// O(N) operation - use sparingly, mainly for monitoring.
/// Returns only alive candidates (within timeout).
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `current_time` - Current time for alive check
///
/// # Returns
/// Vector of alive candidate info, sorted by versionstamp
pub async fn list_candidates<T>(
    txn: &T,
    subspace: &Subspace,
    current_time: Duration,
) -> Result<Vec<CandidateInfo>>
where
    T: Deref<Target = Transaction>,
{
    let config = read_config(txn, subspace).await?;
    let (start, end) = candidates_range(subspace);
    let candidates_subspace = subspace.subspace(&(CANDIDATES_PREFIX,));

    let mut alive_candidates = Vec::new();

    let range = RangeOption::from((start, end));
    let mut kvs = txn.get_ranges_keyvalues(range, false);

    while let Some(kv) = kvs.next().await {
        let kv = kv?;

        // Extract process_id from key
        let key_tuple: (String,) = candidates_subspace.unpack(kv.key())?;
        let process_id = key_tuple.0;

        // Unpack value: (priority, timestamp_nanos, versionstamp)
        let tuple: (i32, u64, Versionstamp) = unpack(kv.value())?;

        let candidate = CandidateInfo {
            process_id,
            priority: tuple.0,
            last_heartbeat_nanos: tuple.1,
            versionstamp: *tuple.2.as_bytes(),
        };

        // Filter by alive status
        if candidate.is_alive(current_time, config.candidate_timeout) {
            alive_candidates.push(candidate);
        }
    }

    // Sort by versionstamp for consistent ordering
    alive_candidates.sort_by(|a, b| a.versionstamp.cmp(&b.versionstamp));

    Ok(alive_candidates)
}

/// Remove dead candidates
///
/// O(N) operation - should be called by leader periodically.
/// Removes candidates that haven't sent heartbeat within timeout.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `current_time` - Current time for timeout check
///
/// # Returns
/// Count of evicted candidates
pub async fn evict_dead_candidates<T>(
    txn: &T,
    subspace: &Subspace,
    current_time: Duration,
) -> Result<usize>
where
    T: Deref<Target = Transaction>,
{
    let config = read_config(txn, subspace).await?;
    let (start, end) = candidates_range(subspace);
    let candidates_subspace = subspace.subspace(&(CANDIDATES_PREFIX,));

    let mut evicted = 0;

    let range = RangeOption::from((start, end));
    let mut kvs = txn.get_ranges_keyvalues(range, false);

    while let Some(kv) = kvs.next().await {
        let kv = kv?;

        // Unpack value: (priority, timestamp_nanos, versionstamp)
        let tuple: (i32, u64, Versionstamp) = unpack(kv.value())?;

        // Extract process_id for CandidateInfo
        let key_tuple: (String,) = candidates_subspace.unpack(kv.key())?;

        let candidate = CandidateInfo {
            process_id: key_tuple.0,
            priority: tuple.0,
            last_heartbeat_nanos: tuple.1,
            versionstamp: *tuple.2.as_bytes(),
        };

        if !candidate.is_alive(current_time, config.candidate_timeout) {
            txn.clear(kv.key());
            evicted += 1;
        }
    }

    Ok(evicted)
}

// ============================================================================
// HIGH-LEVEL CONVENIENCE API
// ============================================================================

/// Run a complete election cycle
///
/// Combines candidate heartbeat + leadership claim in one operation.
/// This is what most users should call in their main loop.
///
/// # Arguments
/// * `txn` - The FoundationDB transaction
/// * `subspace` - The subspace for storing election data
/// * `process_id` - Unique identifier for this process
/// * `my_priority` - This process's priority
/// * `current_time` - Current time
///
/// # Returns
/// `ElectionResult::Leader` if this process is leader, `ElectionResult::Follower` otherwise
///
/// # Example
/// ```ignore
/// loop {
///     let result = db.run(|txn| {
///         run_election_cycle(&txn, &subspace, &my_id, my_priority, now())
///     }).await?;
///
///     match result {
///         ElectionResult::Leader(state) => {
///             // Do leader work
///         }
///         ElectionResult::Follower(Some(leader)) => {
///             // Follow the leader
///         }
///         ElectionResult::Follower(None) => {
///             // No leader yet, retry
///         }
///     }
///
///     sleep(config.heartbeat_interval).await;
/// }
/// ```
pub async fn run_election_cycle<T>(
    txn: &T,
    subspace: &Subspace,
    process_id: &str,
    my_priority: i32,
    current_time: Duration,
) -> Result<ElectionResult>
where
    T: Deref<Target = Transaction>,
{
    // 1. Send heartbeat as candidate
    heartbeat_candidate(txn, subspace, process_id, my_priority, current_time).await?;

    // 2. Try to claim/maintain leadership
    match try_claim_leadership(txn, subspace, process_id, my_priority, current_time).await? {
        Some(state) => Ok(ElectionResult::Leader(state)),
        None => {
            let current_leader = get_leader(txn, subspace, current_time).await?;
            Ok(ElectionResult::Follower(current_leader))
        }
    }
}
