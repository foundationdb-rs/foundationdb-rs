use crate::events::key_range::KeyRange;
use crate::events::mutation::Mutation;
use crate::events::{parse_protocol_if_greater_than, parse_protocol_if_greater_than_with_flag};
use crate::parse::ParseWithProtocolVersion;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion63, ProtocolVersion71};
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

pub const SPAN_ID_SIZE: usize = 16;

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
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
    /// Parses a `CommitInfo` event from the given `Scanner` and `protocol_version`.
    ///
    /// The `CommitInfo` event contains latency, mutation count, commit bytes, commit a version,
    /// read conflict range, write conflict range, mutations, read a snapshot version, report conflicting keys,
    /// lock aware, and span ID of the profiling event.
    ///
    /// * The `commit_version` field is only present if `protocol_version` is greater than
    /// `ProtocolVersion63`
    /// * The `report_conflicting_keys` field is only present if `protocol_version` is
    /// greater than `ProtocolVersion63`
    /// * The `lock_aware` field is only present if `protocol_version` is
    /// greater than `ProtocolVersion71`
    /// * The `span_id` field is only present if `protocol_version` is
    /// greater than `ProtocolVersion71`.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, or if
    /// the `protocol_version` is invalid, an error is returned.
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

        // Report-conflicting keys is only present if a protocol version is greater than 63
        let report_conflicting_keys =
            parse_protocol_if_greater_than(scanner, protocol_version, &ProtocolVersion63).await?;

        // Lock aware is only present if a protocol version is greater than 71
        let lock_aware =
            parse_protocol_if_greater_than(scanner, protocol_version, &ProtocolVersion71).await?;

        // Span ID is only present if a protocol version is greater than 71
        let span_id =
            parse_protocol_if_greater_than_with_flag(scanner, protocol_version, &ProtocolVersion71)
                .await?;

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
