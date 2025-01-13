use crate::errors::ParseError;
use crate::scanner::Scanner;
use core::mem::size_of;
use std::error::Error;

/// Various datatype bytes space
const DOUBLE_BYTES_SIZE: usize = size_of::<f64>();
const INTEGER_BYTES_SIZE: usize = size_of::<u32>();
const LONG_BYTES_SIZE: usize = size_of::<u64>();
const SHORT_BYTES_SIZE: usize = size_of::<u8>();

/// This module provides parsing traits `Parse` and `ParseWithProtocolVersion`
/// for asynchronous deserialization of various data types from a `Scanner`,
/// utilizing custom implementations and macros for efficient byte buffer processing.
#[async_trait::async_trait]
pub trait Parse: Sized {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>>;
}

/// A macro for implementing the `Parse` trait for a given type.
///
/// This macro simplifies the implementation of the `Parse` trait for types
/// that can be deserialized from a fixed-size byte buffer using the `Scanner`.
///
/// # Parameters
///
/// - `$x`: The type for which the `Parse` trait will be implemented (e.g., `f64`, `u32`).
/// - `$type`: The type whose `from_le_bytes` function will be used to perform
///   the deserialization from the byte buffer.
/// - `$size`: The number of bytes required to represent the type in memory.
///
/// # Behavior
///
/// The macro generates the implementation so that:
/// 1. It allocates a buffer of `$size` bytes.
/// 2. Extracts the bytes from the `Scanner`'s remaining data.
/// 3. Advances the `Scanner`'s internal cursor by `$size`.
/// 4. Converts the buffer into the target type `$x` using `$type::from_le_bytes()`.
///
/// # Errors
///
/// If the `Scanner` does not have enough remaining bytes to extract `$size` bytes,
/// the implementation will panic or exhibit undefined behavior due to bounds violations.
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
