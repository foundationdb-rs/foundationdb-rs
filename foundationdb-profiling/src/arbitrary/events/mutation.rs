use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::{Mutation, MutationType};

impl ToBytes for MutationType {
    /// Serializes a `MutationType` into the given `scanner` writer.
    ///
    /// This method serializes the given `MutationType` as a single byte
    /// into the given `scanner` writer.
    ///
    /// # Errors
    ///
    /// If the underlying `scanner` fails to write the expected byte, an
    /// error is returned.
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        (*self as u8).to_bytes(scanner)
    }
}

impl ToBytes for Mutation {
    /// Serializes a `Mutation` event into the given `scanner` writer.
    ///
    /// The `Mutation` event contains the mutation type, parameter one, and
    /// parameter two of the profiling event.
    ///
    /// # Errors
    ///
    /// If the underlying `scanner` fails to write the expected fields, an
    /// error is returned.
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let Mutation {
            mutation_type,
            parameter_one,
            parameter_two,
        } = self;
        mutation_type.to_bytes(scanner)?;
        parameter_one.to_bytes(scanner)?;
        parameter_two.to_bytes(scanner)?;
        Ok(())
    }
}
