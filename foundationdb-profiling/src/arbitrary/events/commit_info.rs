use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::{ToBytes, ToBytesWithProtocolVersion};
use crate::arbitrary::writer::Writer;
use crate::events::CommitInfo;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion63, ProtocolVersion71};

impl ToBytesWithProtocolVersion for CommitInfo {
    /// Serializes a `CommitInfo` event into the given `writer` using the
    /// specified `protocol_version`.
    ///
    /// The `CommitInfo` event contains latency, mutation count, commit bytes,
    /// commit version, read conflict range, write conflict range, mutations, read
    /// snapshot version, report conflicting keys, lock aware, and span ID of the
    /// profiling event.
    ///
    /// The `commit_version` field is only present if `protocol_version` is
    /// greater than `ProtocolVersion63`.
    ///
    /// The `report_conflicting_keys` and `lock_aware` fields are only present if
    /// `protocol_version` is greater than `ProtocolVersion63`.
    ///
    /// The `span_id` field is only present if `protocol_version` is greater than
    /// `ProtocolVersion71`.
    ///
    /// # Errors
    ///
    /// If the underlying `writer` fails to write the expected fields, an error
    /// is returned.
    fn to_bytes_with_protocol_version(
        &self,
        writer: &mut Writer<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<(), ToBytesError> {
        let CommitInfo {
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
        } = self;
        latency.to_bytes(writer)?;
        mutation_count.to_bytes(writer)?;
        commit_bytes.to_bytes(writer)?;
        if protocol_version > &ProtocolVersion63 {
            commit_version.to_bytes(writer)?;
        }
        read_conflict_range.to_bytes(writer)?;
        write_conflict_range.to_bytes(writer)?;
        mutations.to_bytes(writer)?;
        read_snapshot_version.to_bytes(writer)?;
        if protocol_version > &ProtocolVersion63 {
            report_conflicting_keys.to_bytes(writer)?;
            lock_aware.to_bytes(writer)?;
            if protocol_version > &ProtocolVersion71 {
                span_id.to_bytes(writer)?;
            }
        }
        Ok(())
    }
}
