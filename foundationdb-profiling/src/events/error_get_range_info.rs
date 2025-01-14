use crate::events::key_range::KeyRange;
use crate::parse::ParseWithProtocolVersion;
use crate::protocol_version::ProtocolVersion;
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub struct ErrorGetRangeInfo {
    pub(crate) error_code: u32,
    pub(crate) key: KeyRange,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for ErrorGetRangeInfo {
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        _protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(ErrorGetRangeInfo {
            error_code: scanner.parse().await?,
            key: scanner.parse().await?,
        })
    }
}
