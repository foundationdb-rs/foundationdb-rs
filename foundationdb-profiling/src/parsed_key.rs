use crate::parse::Parse;
use crate::scanner::Scanner;
use crate::PROFILE_PREFIX;
use foundationdb::tuple::{Bytes, Versionstamp};
use futures_util::AsyncReadExt;
use std::error::Error;
use std::fmt::{Debug, Formatter};

/// Define the number of bytes taken by the '/'
/// separator, so 1 byte
const SEPARATOR_BYTES_SIZE: usize = 1;

/// VersionStamp is defined as Big endian 10-byte integer.
/// First/high 8 bytes are a database version, next two are batch version.
/// For a total of 10 bytes
const VERSIONSTAMP_BYTES_SIZE: usize = 10;

/// Transaction ID is defined as 16 bytes identifier
const TRANSACTION_ID_BYTES_SIZE: usize = 16;

/// Chunk number or total chunk number are
/// defined as BigEndian 4 bytes integer
const CHUNK_FIELD_SIZE: usize = 4;

/// The `TransactionId` struct encapsulates the unique 16-byte identifier for a
/// transaction used in FoundationDB's profiling system.
///
/// This identifier is part of the database key structure and is used to group
/// events associated with a specific transaction. The structure is parsed from
/// the raw key data as part of the profiling analyzer.
///
/// # Internal Representation
/// Internally, this struct wraps a `Vec<u8>` which contains exactly 16 bytes.
/// The fixed-size representation ensures compatibility with FoundationDB's
/// schema for profiling-related keys.
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

/// A struct representing a chunk field of a parsed key.
/// Can be either a total chunk number or a chunk number
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

/// A struct that represents a logical version, encapsulating a `Versionstamp`.
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
        Ok(Version(Versionstamp::complete(buffer, 0)))
    }
}

/// ProfilingParsedKey represents a structured result of a key parsed from the database.
///
/// # Fields
/// - `transaction_id`: The unique identifier for the transaction.
/// - `version`: The version of the database associated with the key.
/// - `current_chunk`: The current chunk index part of the parsed key.
/// - `total_chunk`: The total number of chunks related to the key.
pub struct ProfilingParsedKey {
    pub transaction_id: TransactionId,
    version: Version,
    current_chunk: usize,
    pub total_chunk: usize,
}

impl ProfilingParsedKey {
    fn new(
        version: Version,
        transaction_id: TransactionId,
        current_chunk: usize,
        total_chunk: usize,
    ) -> Self {
        ProfilingParsedKey {
            version,
            transaction_id,
            current_chunk,
            total_chunk,
        }
    }
}

impl Debug for ProfilingParsedKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedKey")
            .field("version", &self.version)
            .field("transaction_id", &self.transaction_id)
            .field("chunk_id", &self.current_chunk)
            .field("chunk_total", &self.total_chunk)
            .finish()
    }
}

/// Parses a database key into a `ParsedKey` structure.
///
/// # Arguments
///
/// - `key`: The raw byte slice representing the database key.
///
/// # Returns
///
/// A `Result` containing a `ParsedKey` if parsing is successful, or an error
/// if the parsing fails.
///
/// The function expects the key to follow the specific format:
///    - A prefix (`PROFILE_PREFIX`) which is skipped.
///    - A `Version` field parsed from the key.
///    - A separator (`/`), accounted for by a single byte (defined by `SEPARATOR_BYTES_SIZE`).
///    - A `TransactionId` field.
///    - Another separator (`/`).
///    - Two `ChunkField`s representing chunk number and total chunks.
///    - A final separator (`/`).
/// Parsing converts this data into a `ParsedKey` structure which represents
/// the fields and their respective values in a structured format.
///
/// # Errors
///
/// This function can return errors in cases such as:
/// - Invalid format of the provided key.
/// - Failure to read the key's fields due to incorrect key length or content.
pub async fn parse_key(key: &[u8]) -> Result<ProfilingParsedKey, Box<dyn Error + Send + Sync>> {
    let mut scanner = Scanner::new(key);
    scanner.bump_by(PROFILE_PREFIX.len());
    let version = Version::parse(&mut scanner).await?;
    scanner.bump_by(SEPARATOR_BYTES_SIZE);
    let transaction_id = TransactionId::parse(&mut scanner).await?;
    scanner.bump_by(SEPARATOR_BYTES_SIZE);
    let chunk_number = ChunkField::parse(&mut scanner).await?.0;
    let chunk_total = ChunkField::parse(&mut scanner).await?.0;
    scanner.bump_by(SEPARATOR_BYTES_SIZE);

    let parsed_key = ProfilingParsedKey::new(version, transaction_id, chunk_number, chunk_total);

    Ok(parsed_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_parse_key() {
        let key = b"\xff\x02/fdbClientInfo/client_latency/\x19\x5a\x63\xa7\x00\x2f\x3b\xa1\xd8\x00/\x5a\x0c\xd7\xad\x1f\x66\x2c\xb4\xa6\x72\xdb\x45\x3a\x68\xf5\xc1/\x00\x00\x00\x01\x00\x00\x00\x03";
        let parsed_key = parse_key(key).await.expect("Unable to parse key");
        assert_eq!(
            parsed_key.transaction_id.0,
            [
                0x5a, 0x0c, 0xd7, 0xad, 0x1f, 0x66, 0x2c, 0xb4, 0xa6, 0x72, 0xdb, 0x45, 0x3a, 0x68,
                0xf5, 0xc1
            ]
        );
        assert_eq!(
            parsed_key.version.0.transaction_version(),
            b"\x19\x5a\x63\xa7\x00\x2f\x3b\xa1\xd8\x00"
        );
        assert_eq!(parsed_key.current_chunk, 1);
        assert_eq!(parsed_key.total_chunk, 3);
    }
}
