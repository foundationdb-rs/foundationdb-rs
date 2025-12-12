//! Simulation workloads for testing FoundationDB recipes
//!
//! This crate provides simulation workloads for testing FoundationDB recipes
//! under deterministic simulation.

mod leader_election;

pub use leader_election::LeaderElectionWorkload;

use foundationdb_simulation::register_workload;

register_workload!(LeaderElectionWorkload);
