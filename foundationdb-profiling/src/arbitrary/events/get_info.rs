use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::GetInfo;

impl ToBytes for GetInfo {
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let GetInfo {
            latency,
            value_size,
            key,
        } = self;
        latency.to_bytes(scanner)?;
        value_size.to_bytes(scanner)?;
        key.to_bytes(scanner)?;
        Ok(())
    }
}
