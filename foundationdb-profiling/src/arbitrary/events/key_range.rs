use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::KeyRange;

impl ToBytes for KeyRange {
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let KeyRange { start, end } = self;
        start.to_bytes(scanner)?;
        end.to_bytes(scanner)?;
        Ok(())
    }
}
