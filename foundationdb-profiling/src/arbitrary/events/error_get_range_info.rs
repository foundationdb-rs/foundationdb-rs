use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::ErrorGetRangeInfo;

impl ToBytes for ErrorGetRangeInfo {
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let ErrorGetRangeInfo { error_code, key } = self;
        error_code.to_bytes(scanner)?;
        key.to_bytes(scanner)?;
        Ok(())
    }
}