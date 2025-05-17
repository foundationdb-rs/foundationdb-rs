use crate::parse::ParseWithProtocolVersion;
use crate::protocol_version::ProtocolVersion;
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct ErrorGetInfo {
    pub(crate) error_code: u32,
    pub(crate) key: Vec<u8>,
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for ErrorGetInfo {
    /// Parses an `ErrorGetInfo` event from the given `Scanner` and `protocol_version`.
    ///
    /// The `ErrorGetInfo` event contains the error code and key of the profiling event.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, or if
    /// the `protocol_version` is invalid, an error is returned.
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        _protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(ErrorGetInfo {
            error_code: scanner.parse().await?,
            key: scanner.parse().await?,
        })
    }
}
