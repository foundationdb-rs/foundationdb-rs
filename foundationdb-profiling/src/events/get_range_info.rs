use crate::events::key_range::KeyRange;
use crate::parse::ParseWithProtocolVersion;
use crate::protocol_version::ProtocolVersion;
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct GetRangeInfo {
    pub(crate) latency: f64,
    pub(crate) range_size: u32,
    pub(crate) key_range: KeyRange,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for GetRangeInfo {
    /// Parses a `GetRangeInfo` event from the given `Scanner` and `protocol_version`.
    ///
    /// The `GetRangeInfo` event contains the latency, range size, and key range of the profiling event.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, or if
    /// the `protocol_version` is invalid, an error is returned.
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
