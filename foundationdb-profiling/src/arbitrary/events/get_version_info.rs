use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::{ToBytes, ToBytesWithProtocolVersion};
use crate::arbitrary::writer::Writer;
use crate::events::GetVersionInfo;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion62, ProtocolVersion63};

impl ToBytesWithProtocolVersion for GetVersionInfo {
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
