use crate::profiling_events::parse::Parse;
use crate::profiling_events::scanner::Scanner;
use crate::tuple::{Bytes, Versionstamp};
use futures_util::AsyncReadExt;
use std::error::Error;
use std::fmt::{Debug, Formatter};

pub struct ParsedKey {
    pub transaction_id: TransactionId,
    version: Version,
    current_chunk: usize,
    pub total_chunk: usize,
}

impl ParsedKey {
    fn new(
        version: Version,
        transaction_id: TransactionId,
        current_chunk: usize,
        total_chunk: usize,
    ) -> Self {
        ParsedKey {
            version,
            transaction_id,
            current_chunk,
            total_chunk,
        }
    }
}

impl Debug for ParsedKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedKey")
            .field("version", &self.version)
            .field("transaction_id", &self.transaction_id)
            .field("chunk_id", &self.current_chunk)
            .field("chunk_total", &self.total_chunk)
            .finish()
    }
}

pub async fn parse_key(key: &[u8]) -> Result<ParsedKey, Box<dyn Error + Send + Sync>> {
    let mut scanner = Scanner::new(key);
    scanner.bump_by(PROFILE_PREFIX.len());
    let version = Version::parse(&mut scanner).await?;
    scanner.bump_by(SEPARATOR_BYTES_SIZE);
    let transaction_id = TransactionId::parse(&mut scanner).await?;
    scanner.bump_by(SEPARATOR_BYTES_SIZE);
    let chunk_number = ChunkField::parse(&mut scanner).await?.0;
    let chunk_total = ChunkField::parse(&mut scanner).await?.0;
    scanner.bump_by(SEPARATOR_BYTES_SIZE);

    let parsed_key = ParsedKey::new(version, transaction_id, chunk_number, chunk_total);

    Ok(parsed_key)
}

const TRANSACTION_ID_BYTES_SIZE: usize = 16;
const CHUNK_FIELD_SIZE: usize = 4;

pub struct TransactionId(Vec<u8>);

impl Debug for TransactionId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let transaction_id = Bytes::from(&self.0[..]);
        write!(f, "{transaction_id}")
    }
}

#[async_trait::async_trait]
impl Parse for TransactionId {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut buf = [0_u8; TRANSACTION_ID_BYTES_SIZE];
        scanner.read_exact(&mut buf).await?;
        Ok(TransactionId(buf.to_vec()))
    }
}

struct ChunkField(usize);

#[async_trait::async_trait]
impl Parse for ChunkField {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut buf = [0_u8; CHUNK_FIELD_SIZE];
        scanner.read_exact(&mut buf).await?;
        let data = u32::from_be_bytes(buf) as usize;
        Ok(ChunkField(data))
    }
}

pub const PROFILE_PREFIX: &[u8; 32] = b"\xff\x02/fdbClientInfo/client_latency/";
const SEPARATOR_BYTES_SIZE: usize = 1;

struct Version(Versionstamp);

impl Debug for Version {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let version = self.0.transaction_version()[..8].to_vec();
        let version = u64::from_be_bytes(version.try_into().map_err(|_err| std::fmt::Error)?);
        write!(f, "{version}")
    }
}

#[async_trait::async_trait]
impl Parse for Version {
    async fn parse(scanner: &mut Scanner<'_>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut buffer = [0_u8; VERSIONSTAMP_BYTES_SIZE];
        scanner.read_exact(&mut buffer).await?;
        Ok(Version(Versionstamp::complete(buffer, 45)))
    }
}

const VERSIONSTAMP_BYTES_SIZE: usize = 10;
