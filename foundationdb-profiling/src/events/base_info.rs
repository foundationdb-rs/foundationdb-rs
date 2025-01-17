use crate::parse::{Parse, ParseWithProtocolVersion};
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion63, ProtocolVersion71};
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub struct BaseInfo {
    pub(crate) start_timestamp: f64,
    pub(crate) dc_id: Option<String>,
    pub(crate) tenant: Option<String>,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for BaseInfo {
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let start_timestamp = f64::parse(scanner).await?;

        let dc_id = if protocol_version > &ProtocolVersion63 {
            scanner.parse().await?
        } else {
            None
        };

        let tenant = if protocol_version > &ProtocolVersion71 {
            if bool::parse(scanner).await? {
                scanner.parse().await?
            } else {
                None
            }
        } else {
            None
        };
        Ok(BaseInfo {
            start_timestamp,
            dc_id,
            tenant,
        })
    }
}
