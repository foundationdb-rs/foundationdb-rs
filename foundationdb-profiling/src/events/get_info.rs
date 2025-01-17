use crate::parse::{Parse, ParseWithProtocolVersion};
use crate::protocol_version::ProtocolVersion;
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub struct GetInfo {
    pub(crate) latency: f64,
    pub(crate) value_size: u32,
    pub(crate) key: String,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for GetInfo {
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        _protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(GetInfo {
            latency: scanner.parse().await?,
            value_size: scanner.parse().await?,
            key: scanner.parse().await?,
        })
    }
}
