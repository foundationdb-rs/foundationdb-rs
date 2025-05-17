use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::{ToBytes, ToBytesWithProtocolVersion};
use crate::arbitrary::writer::Writer;
use crate::events::BaseInfo;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion63, ProtocolVersion71};

impl ToBytesWithProtocolVersion for BaseInfo {
    /// Serializes a `BaseInfo` event to the given `writer` using the
    /// provided `protocol_version`.
    ///
    /// The `start_timestamp` field is always written, and the `dc_id` field
    /// is written if the `protocol_version` is greater than `ProtocolVersion63`.
    /// The `tenant` field is written if the `protocol_version` is greater than
    /// `ProtocolVersion71`.
    ///
    /// # Errors
    ///
    /// If the underlying `writer` fails to write the expected fields, an
    /// error is returned.
    fn to_bytes_with_protocol_version(
        &self,
        writer: &mut Writer<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<(), ToBytesError> {
        let BaseInfo {
            start_timestamp,
            dc_id,
            tenant,
        } = self;

        start_timestamp.to_bytes(writer)?;
        if protocol_version > &ProtocolVersion63 {
            dc_id.to_bytes(writer)?;
        }
        if protocol_version > &ProtocolVersion71 {
            tenant.to_bytes(writer)?;
        }
        Ok(())
    }
}
