use crate::parse::Parse;
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct KeyRange {
    pub(crate) start: Vec<u8>,
    pub(crate) end: Vec<u8>,
}

#[async_trait::async_trait]
impl Parse for KeyRange {
    /// Parses a `KeyRange` event from the given `Scanner`.
    ///
    /// A `KeyRange` event contains the start and end of the key range.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, an error is returned.
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(KeyRange {
            start: scanner.parse().await?,
            end: scanner.parse().await?,
        })
    }
}
