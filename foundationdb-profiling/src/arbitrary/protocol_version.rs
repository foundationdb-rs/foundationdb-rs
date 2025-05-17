use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::protocol_version::ProtocolVersion;

impl ToBytes for ProtocolVersion {
    /// Serialize a `ProtocolVersion` enum into the given `Writer`.
    ///
    /// The byte representation of each variant is specified in the FoundationDB
    /// wire protocol documentation.
    ///
    /// # Errors
    ///
    /// If the underlying writer fails to write the expected fields, an error is
    /// returned.
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
        match self {
            ProtocolVersion::ProtocolVersion52 => 0x0FDB00A552000001_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion60 => 0x0FDB00A570010001_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion61 => 0x0FDB00B061060001_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion62 => 0x0FDB00B062010001_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion63 => 0x0FDB00B063010001_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion70 => 0x0FDB00B070010001_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion71 => 0x0FDB00B071010000_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion72 => 0x0FDB00B072000000_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion73 => 0x0FDB00B073000000_u64.to_bytes(writer)?,
            ProtocolVersion::ProtocolVersion74 => 0x0FDB00B074000000_u64.to_bytes(writer)?,
        };
        Ok(())
    }
}
