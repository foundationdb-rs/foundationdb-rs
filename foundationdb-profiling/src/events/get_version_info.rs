use crate::events::parse_protocol_if_greater_than;
use crate::parse::ParseWithProtocolVersion;
use crate::protocol_version::ProtocolVersion;
use crate::protocol_version::ProtocolVersion::{ProtocolVersion62, ProtocolVersion63};
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct GetVersionInfo {
    pub(crate) latency: f64,
    pub(crate) transaction_priority_type: Option<u32>,
    pub(crate) read_version: Option<u64>,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for GetVersionInfo {
    /// Parses a `GetVersionInfo` event from the given `Scanner` and `protocol_version`.
    ///
    /// The `GetVersionInfo` event contains the latency, transaction priority type, and read version of the profiling event.
    ///
    /// The `transaction_priority_type` field is only present if `protocol_version` is greater than `ProtocolVersion62`.
    ///
    /// The `read_version` field is only present if `protocol_version` is greater than `ProtocolVersion63`.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, or if
    /// the `protocol_version` is invalid, an error is returned.
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let latency = scanner.parse().await?;

        // The transaction priority type field is only present if `protocol_version` is greater than `ProtocolVersion62`.
        let transaction_priority_type =
            parse_protocol_if_greater_than(scanner, protocol_version, &ProtocolVersion62).await?;

        // The read version field is only present if `protocol_version` is greater than `ProtocolVersion63`.
        let read_version =
            parse_protocol_if_greater_than(scanner, protocol_version, &ProtocolVersion63).await?;

        Ok(GetVersionInfo {
            latency,
            transaction_priority_type,
            read_version,
        })
    }
}
