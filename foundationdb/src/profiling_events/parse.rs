use crate::profiling_events::errors::ParseError;
use crate::profiling_events::protocol_version::ProtocolVersion;
use crate::profiling_events::scanner::Scanner;
use std::error::Error;

const DOUBLE_BYTES_SIZE: usize = size_of::<f64>();
const INTEGER_BYTES_SIZE: usize = size_of::<u32>();
const LONG_BYTES_SIZE: usize = size_of::<u64>();
const SHORT_BYTES_SIZE: usize = size_of::<u8>();

#[async_trait::async_trait]
pub trait Parse: Sized {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>>;
}

#[async_trait::async_trait]
pub trait ParseWithProtocolVersion: Sized {
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>>;
}

macro_rules! impl_parse {
    ($x: ty, $type: ident, $size: expr) => {
        #[async_trait::async_trait]
        impl Parse for $x {
            async fn parse(
                scanner: &mut Scanner<'_>,
            ) -> Result<Self, Box<dyn Error + Send + Sync>> {
                let mut buf = [0_u8; $size];
                buf.copy_from_slice(&scanner.remaining()[..$size]);
                scanner.bump_by($size);
                Ok($type::from_le_bytes(buf))
            }
        }
    };
}

#[async_trait::async_trait]
impl<T: Parse> Parse for Option<T> {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Some(T::parse(scanner).await?))
    }
}

#[async_trait::async_trait]
impl<T: Parse + Send> Parse for Vec<T> {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let size = u32::parse(scanner).await? as usize;
        let mut result = Vec::with_capacity(size);
        for _ in 0..size {
            result.push(scanner.parse().await?);
        }
        Ok(result)
    }
}

#[async_trait::async_trait]
impl<T: Parse + Copy, const N: usize> Parse for [T; N] {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let result = [T::parse(scanner).await?; N];
        Ok(result)
    }
}

#[async_trait::async_trait]
impl Parse for bool {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(u8::parse(scanner).await? != 0)
    }
}

#[async_trait::async_trait]
impl Parse for String {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let size = u32::parse(scanner).await? as usize;
        Ok(String::from_utf8(scanner.remaining()[..size].to_vec())
            .map_err(ParseError::Utf8Decode)?)
    }
}

impl_parse!(f64, f64, DOUBLE_BYTES_SIZE);
impl_parse!(u32, u32, INTEGER_BYTES_SIZE);
impl_parse!(u64, u64, LONG_BYTES_SIZE);
impl_parse!(u8, u8, SHORT_BYTES_SIZE);
