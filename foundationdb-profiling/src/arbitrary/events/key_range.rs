use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::KeyRange;

impl ToBytes for KeyRange {
    /// Serialize this `KeyRange` into the given `Writer`.
    ///
    /// # Errors
    ///
    /// If writing to the underlying writer fails, returns an error.
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let KeyRange { start, end } = self;
        start.to_bytes(scanner)?;
        end.to_bytes(scanner)?;
        Ok(())
    }
}
