use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::GetRangeInfo;

impl ToBytes for GetRangeInfo {
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let GetRangeInfo {
            latency,
            range_size,
            key_range,
        } = self;
        latency.to_bytes(scanner)?;
        range_size.to_bytes(scanner)?;
        key_range.to_bytes(scanner)?;
        Ok(())
    }
}
