use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::ToBytes;
use crate::arbitrary::writer::Writer;
use crate::events::{Mutation, MutationType};

impl ToBytes for MutationType {
    fn to_bytes(&self, scanner: &mut Writer<'_>) -> Result<(), ToBytesError> {
        (*self as u8).to_bytes(scanner)
    }
}

impl ToBytes for Mutation {
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
