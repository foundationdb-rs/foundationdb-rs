use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::{ToBytes, ToBytesWithProtocolVersion};
use crate::arbitrary::writer::Writer;
use crate::events::GetVersionInfo;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion62, ProtocolVersion63};

impl ToBytesWithProtocolVersion for GetVersionInfo {
    /// Serializes a `GetVersionInfo` event to the given `writer` using the
    /// specified `protocol_version`.
    ///
    /// The `latency` field is always written.
    ///
    /// The `transaction_priority_type` field is only written if
    /// `protocol_version` is greater than `ProtocolVersion62`.
    ///
    /// The `read_version` field is only written if `protocol_version` is
    /// greater than `ProtocolVersion63`.
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
        let GetVersionInfo {
            latency,
            transaction_priority_type,
            read_version,
        } = self;
        latency.to_bytes(writer)?;
        if protocol_version > &ProtocolVersion62 {
            transaction_priority_type.to_bytes(writer)?;
        }
        if protocol_version > &ProtocolVersion63 {
            read_version.to_bytes(writer)?;
        }
        Ok(())
    }
}
