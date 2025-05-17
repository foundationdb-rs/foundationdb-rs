use crate::parse::ParseWithProtocolVersion;
use crate::protocol_version::ProtocolVersion;
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct GetInfo {
    pub(crate) latency: f64,
    pub(crate) value_size: u32,
    pub(crate) key: String,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for GetInfo {
    /// Parses a `GetInfo` event from the given `Scanner` and `protocol_version`.
    ///
    /// The `GetInfo` event contains the latency, value size, and key of the profiling event.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, or if
    /// the `protocol_version` is invalid, an error is returned.
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
