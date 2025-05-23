use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::ErrorGetInfo;

impl ToBytes for ErrorGetInfo {
    /// Serialize this `ErrorGetInfo` into the given `Writer`.
    ///
    /// # Errors
    ///
    /// If writing to the underlying writer fails, returns an error.
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let ErrorGetInfo { error_code, key } = self;
        error_code.to_bytes(scanner)?;
        key.to_bytes(scanner)
    }
}
