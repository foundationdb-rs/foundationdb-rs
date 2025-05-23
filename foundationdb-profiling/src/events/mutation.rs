use crate::errors::ParseError;
use crate::parse::Parse;
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
#[repr(u8)]
pub enum MutationType {
    SetValue = 0,
    ClearRange = 1,
    AddValue = 2,
    DebugKeyRange = 3,
    DebugKey = 4,
    NoOp = 5,
    And = 6,
    Or = 7,
    Xor = 8,
    AppendIfFits = 9,
    AvailableForReuse = 10,
    ReservedForLogProtocolMessage = 11,
    Max = 12,
    Min = 13,
    SetVersionStampedKey = 14,
    SetVersionStampedValue = 15,
    ByteMin = 16,
    ByteMax = 17,
    MinV2 = 18,
    AndV2 = 19,
    CompareAndClear = 20,
}

impl TryFrom<u8> for MutationType {
    type Error = ParseError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::SetValue),
            1 => Ok(Self::ClearRange),
            2 => Ok(Self::AddValue),
            3 => Ok(Self::DebugKeyRange),
            4 => Ok(Self::DebugKey),
            5 => Ok(Self::NoOp),
            6 => Ok(Self::And),
            7 => Ok(Self::Or),
            8 => Ok(Self::Xor),
            9 => Ok(Self::AppendIfFits),
            10 => Ok(Self::AvailableForReuse),
            11 => Ok(Self::ReservedForLogProtocolMessage),
            12 => Ok(Self::Max),
            13 => Ok(Self::Min),
            14 => Ok(Self::SetVersionStampedKey),
            15 => Ok(Self::SetVersionStampedValue),
            16 => Ok(Self::ByteMin),
            17 => Ok(Self::ByteMax),
            18 => Ok(Self::MinV2),
            19 => Ok(Self::AndV2),
            20 => Ok(Self::CompareAndClear),
            _ => Err(ParseError::UnknownMutationType(value)),
        }
    }
}

#[async_trait::async_trait]
impl Parse for MutationType {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self::try_from(u8::parse(scanner).await?)?)
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct Mutation {
    pub(crate) mutation_type: MutationType,
    pub(crate) parameter_one: Vec<u8>,
    pub(crate) parameter_two: Vec<u8>,
}

#[async_trait::async_trait]
impl Parse for Mutation {
    /// Parses a `Mutation` event from the given `Scanner`.
    ///
    /// The `Mutation` event contains the mutation type, parameter one, and
    /// parameter two of the profiling event.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, an
    /// error is returned.
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            mutation_type: scanner.parse().await?,
            parameter_one: scanner.parse().await?,
            parameter_two: scanner.parse().await?,
        })
    }
}
