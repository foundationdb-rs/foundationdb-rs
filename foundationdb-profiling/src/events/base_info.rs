use crate::events::{parse_protocol_if_greater_than, parse_protocol_if_greater_than_with_flag};
use crate::parse::{Parse, ParseWithProtocolVersion};
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion63, ProtocolVersion71};
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct BaseInfo {
    pub(crate) start_timestamp: f64,
    pub(crate) dc_id: Option<String>,
    pub(crate) tenant: Option<String>,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for BaseInfo {
    /// Parses a `BaseInfo` event from the given `Scanner` and `protocol_version`.
    ///
    /// The `BaseInfo` event contains the start timestamp of the profiling event,
    /// the datacenter ID, and the tenant name of the profiling event.
    ///
    /// The `dc_id` field is only present if `protocol_version` is greater than
    /// `ProtocolVersion63`, and the `tenant` field is only present if
    /// `protocol_version` is greater than `ProtocolVersion71`.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, or if
    /// the `protocol_version` is invalid, an error is returned.
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let start_timestamp = f64::parse(scanner).await?;

        // Datacenter ID is only present if a protocol version is greater than 63
        let dc_id =
            parse_protocol_if_greater_than(scanner, protocol_version, &ProtocolVersion63).await?;

        // Tenant is only present if a protocol version is greater than 71
        let tenant =
            parse_protocol_if_greater_than_with_flag(scanner, protocol_version, &ProtocolVersion71)
                .await?;

        Ok(BaseInfo {
            start_timestamp,
            dc_id,
            tenant,
        })
    }
}
