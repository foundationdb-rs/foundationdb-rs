use crate::errors::ParseError;
use crate::parse::Parse;
use crate::scanner::Scanner;
#[cfg(feature = "fuzzing")]
use arbitrary::Arbitrary;
use futures_util::AsyncReadExt;
use std::error::Error;

const PROTOCOL_VERSION_BYTES_SIZE: usize = 8;

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq)]
#[cfg_attr(feature = "fuzzing", derive(Arbitrary))]
pub enum ProtocolVersion {
    ProtocolVersion52,
    ProtocolVersion60,
    ProtocolVersion61,
    ProtocolVersion62,
    ProtocolVersion63,
    ProtocolVersion70,
    ProtocolVersion71,
    ProtocolVersion72,
    ProtocolVersion73,
    ProtocolVersion74,
}

impl TryFrom<u64> for ProtocolVersion {
    type Error = ParseError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0x0FDB00A552000001 => Ok(ProtocolVersion::ProtocolVersion52),
            0x0FDB00A570010001 => Ok(ProtocolVersion::ProtocolVersion60),
            0x0FDB00B061060001 => Ok(ProtocolVersion::ProtocolVersion61),
            0x0FDB00B062010001 => Ok(ProtocolVersion::ProtocolVersion62),
            0x0FDB00B063010001 => Ok(ProtocolVersion::ProtocolVersion63),
            0x0FDB00B070010001 => Ok(ProtocolVersion::ProtocolVersion70),
            0x0FDB00B071010000 => Ok(ProtocolVersion::ProtocolVersion71),
            0x0FDB00B072000000 => Ok(ProtocolVersion::ProtocolVersion72),
            0x0FDB00B073000000 => Ok(ProtocolVersion::ProtocolVersion73),
            0x0FDB00B074000000 => Ok(ProtocolVersion::ProtocolVersion74),
            _ => Err(ParseError::UnsupportedProtocolVersion(value)),
        }
    }
}

#[async_trait::async_trait]
impl Parse for ProtocolVersion {
    async fn parse(cursor: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut buf = [0_u8; PROTOCOL_VERSION_BYTES_SIZE];
        cursor.read_exact(&mut buf).await?;
        let protocol_version = u64::from_le_bytes(buf);
        Ok(protocol_version.try_into()?)
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol_version::ProtocolVersion::*;

    #[test]
    fn test_version_greater_than_63() {
        for version in [
            ProtocolVersion52,
            ProtocolVersion60,
            ProtocolVersion61,
            ProtocolVersion62,
        ] {
            assert!(ProtocolVersion63 > version)
        }

        for version in [
            ProtocolVersion63,
            ProtocolVersion70,
            ProtocolVersion71,
            ProtocolVersion72,
            ProtocolVersion73,
        ] {
            assert!(ProtocolVersion63 <= version)
        }
    }
}
