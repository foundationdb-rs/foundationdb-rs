use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::writer::Writer;
use crate::protocol_version::ProtocolVersion;
use std::io::Write;

pub trait ToBytes {
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError>;
}

pub trait ToBytesWithProtocolVersion {
    /// Serializes `self` into the given `writer` using the specified `protocol_version`.
    ///
    /// # Errors
    ///
    /// If the underlying `writer` fails to write the expected fields, an error is returned.
    fn to_bytes_with_protocol_version(
        &self,
        writer: &mut Writer<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<(), ToBytesError>;
}

/// Macro to generate an implementation of `ToBytes` for a type.
///
/// The implementation serializes the given type using its `to_le_bytes` method.
///
/// # Parameters
///
/// * `$x`: The type for which an implementation of `ToBytes` will be generated.
macro_rules! impl_to_bytes {
    ($x: ty) => {
        impl ToBytes for $x {
            fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
                writer.write_all(self.to_le_bytes().as_slice())?;
                Ok(())
            }
        }
    };
}

impl_to_bytes!(u8);
impl_to_bytes!(u32);
impl_to_bytes!(u64);
impl_to_bytes!(f64);

impl ToBytes for String {
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
        (self.len() as u32).to_bytes(writer)?;
        writer.write_all(self.as_bytes())?;
        Ok(())
    }
}

impl ToBytes for bool {
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
        if *self {
            1u8.to_bytes(writer)
        } else {
            0u8.to_bytes(writer)
        }
    }
}

impl<T: ToBytes> ToBytes for Option<T> {
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
        if let Some(value) = self {
            value.to_bytes(writer)
        } else {
            Ok(())
        }
    }
}

impl<T: ToBytes> ToBytes for Vec<T> {
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
        (self.len() as u32).to_bytes(writer)?;
        for value in self {
            value.to_bytes(writer)?;
        }
        Ok(())
    }
}

impl<T: ToBytesWithProtocolVersion> ToBytesWithProtocolVersion for Vec<T> {
    fn to_bytes_with_protocol_version(
        &self,
        writer: &mut Writer<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<(), ToBytesError> {
        (self.len() as u32).to_bytes(writer)?;
        for value in self {
            value.to_bytes_with_protocol_version(writer, protocol_version)?;
        }
        Ok(())
    }
}

impl<T: ToBytes + Clone, const N: usize> ToBytes for [T; N] {
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
        self.to_vec().to_bytes(writer)
    }
}
