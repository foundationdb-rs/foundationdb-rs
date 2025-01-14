use crate::events::key_range::KeyRange;
use crate::events::mutation::Mutation;
use crate::parse::{Parse, ParseWithProtocolVersion};
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion63, ProtocolVersion71};
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use std::error::Error;

pub const SPAN_ID_SIZE: usize = 16;

#[derive(Debug)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub struct CommitInfo {
    pub(crate) latency: f64,
    pub(crate) mutation_count: u32,
    pub(crate) commit_bytes: u32,
    pub(crate) commit_version: Option<u64>,
    pub(crate) read_conflict_range: Vec<KeyRange>,
    pub(crate) write_conflict_range: Vec<KeyRange>,
    pub(crate) mutations: Vec<Mutation>,
    pub(crate) read_snapshot_version: u64,
    pub(crate) report_conflicting_keys: Option<bool>,
    pub(crate) lock_aware: Option<bool>,
    pub(crate) span_id: Option<[u8; SPAN_ID_SIZE]>,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for CommitInfo {
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let latency = scanner.parse().await?;
        let mutation_count = scanner.parse().await?;
        let commit_bytes = scanner.parse().await?;
        let commit_version = if protocol_version > &ProtocolVersion63 {
            scanner.parse().await?
        } else {
            None
        };

        let read_conflict_range = scanner.parse().await?;
        let write_conflict_range = scanner.parse().await?;

        let mutations = scanner.parse().await?;

        let read_snapshot_version = scanner.parse().await?;

        let report_conflicting_keys = if protocol_version > &ProtocolVersion63 {
            scanner.parse().await?
        } else {
            None
        };

        let lock_aware = if protocol_version > &ProtocolVersion71 {
            scanner.parse().await?
        } else {
            None
        };

        let span_id = if protocol_version > &ProtocolVersion71 {
            if bool::parse(scanner).await? {
                scanner.parse().await?
            } else {
                None
            }
        } else {
            None
        };

        Ok(CommitInfo {
            latency,
            mutation_count,
            commit_bytes,
            commit_version,
            read_conflict_range,
            write_conflict_range,
            mutations,
            read_snapshot_version,
            report_conflicting_keys,
            lock_aware,
            span_id,
        })
    }
}
