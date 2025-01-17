use crate::parse::ParseWithProtocolVersion;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion62, ProtocolVersion63};
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub struct GetVersionInfo {
    pub(crate) latency: f64,
    pub(crate) transaction_priority_type: Option<u32>,
    pub(crate) read_version: Option<u64>,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for GetVersionInfo {
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let latency = scanner.parse().await?;
        let transaction_priority_type = if protocol_version > &ProtocolVersion62 {
            scanner.parse().await?
        } else {
            None
        };
        let read_version = if protocol_version > &ProtocolVersion63 {
            scanner.parse().await?
        } else {
            None
        };
        Ok(GetVersionInfo {
            latency,
            transaction_priority_type,
            read_version,
        })
    }
}
