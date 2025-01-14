use crate::parse::Parse;
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub struct KeyRange {
    pub(crate) start: Vec<u8>,
    pub(crate) end: Vec<u8>,
}

#[async_trait::async_trait]
impl Parse for KeyRange {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(KeyRange {
            start: scanner.parse().await?,
            end: scanner.parse().await?,
        })
    }
}
