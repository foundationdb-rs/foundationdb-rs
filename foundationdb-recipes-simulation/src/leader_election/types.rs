//! Type definitions for the leader election simulation workload

use std::time::Duration;

/// Base time offset to convert simulation time to realistic timestamps
pub const BASE_OFFSET_SECS: u64 = 1_700_000_000;

/// Default heartbeat timeout in seconds
pub const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Operation types for logging (using i64 for TuplePack compatibility)
pub mod op_types {
    pub const REGISTER: i64 = 1;
    pub const HEARTBEAT: i64 = 2;
    pub const BECAME_LEADER: i64 = 3;
    pub const CONFIG_CHANGE: i64 = 4;
    pub const LEADER_CLEARED: i64 = 5;
    pub const PROCESS_KILLED: i64 = 6;
}

/// Time anomaly types for injection
#[derive(Debug, Clone, Copy)]
pub enum TimeAnomaly {
    Normal,
    Stale,
    Future,
    ExactlyAtTimeout,
    JustBeforeTimeout,
    JustAfterTimeout,
    Zero,
    TimeReversal,
}

/// Process state tracked by the workload
#[derive(Debug, Clone)]
pub struct ProcessState {
    pub uuid: String,
    pub last_heartbeat: Duration,
    pub is_alive: bool,
}
