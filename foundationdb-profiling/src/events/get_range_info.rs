use crate::events::key_range::KeyRange;
use crate::parse::{Parse, ParseWithProtocolVersion};
use crate::protocol_version::ProtocolVersion;
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub struct GetRangeInfo {
    pub(crate) latency: f64,
    pub(crate) range_size: u32,
    pub(crate) key_range: KeyRange,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for GetRangeInfo {
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        _protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            latency: scanner.parse().await?,
            range_size: scanner.parse().await?,
            key_range: scanner.parse().await?,
        })
    }
}
