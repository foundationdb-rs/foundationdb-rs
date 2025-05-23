use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::{ToBytes, ToBytesWithProtocolVersion};
use crate::arbitrary::writer::Writer;
use crate::events::ErrorCommitInfo;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion63, ProtocolVersion71};

impl ToBytesWithProtocolVersion for ErrorCommitInfo {
    /// Serializes an `ErrorCommitInfo` event into the given `writer` using the specified
    /// `protocol_version`.
    ///
    /// The `ErrorCommitInfo` event contains the error code, read conflict range, write
    /// conflict range, mutations, read a snapshot version, report conflicting keys,
    /// lock aware, and span ID of the profiling event.
    ///
    /// The `report_conflicting_keys` field is only present if `protocol_version` is
    /// greater than `ProtocolVersion63`.
    ///
    /// The `lock_aware` field is only present if `protocol_version` is greater than
    /// `ProtocolVersion71`.
    ///
    /// The `span_id` field is only present if `protocol_version` is greater than
    /// `ProtocolVersion71` and the `span_id` is present.
    ///
    /// # Errors
    ///
    /// If the underlying `writer` fails to write the expected fields, an error is returned.
    fn to_bytes_with_protocol_version(
        &self,
        writer: &mut Writer<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<(), ToBytesError> {
        let ErrorCommitInfo {
            error_code,
            read_conflict_range,
            write_conflict_range,
            mutations,
            read_snapshot_version,
            report_conflicting_keys,
            lock_aware,
            span_id,
        } = self;
        error_code.to_bytes(writer)?;
        read_conflict_range.to_bytes(writer)?;
        write_conflict_range.to_bytes(writer)?;
        mutations.to_bytes(writer)?;
        read_snapshot_version.to_bytes(writer)?;
        if protocol_version > &ProtocolVersion63 {
            report_conflicting_keys.to_bytes(writer)?;
        }
        if protocol_version > &ProtocolVersion71 {
            lock_aware.to_bytes(writer)?;
        }

        if let Some(ref span_id) = span_id {
            true.to_bytes(writer)?;
            span_id.to_bytes(writer)?;
        }
        Ok(())
    }
}
