use crate::parsed_key::TransactionId;
use foundationdb::tuple::Bytes;
use std::fmt::{Debug, Formatter};

/// A structure that represents a block of data associated with a specific transaction.
/// This structure provides functionality to manage and track progress as data chunks
/// are added until a target chunk count is reached.
///
/// # Overview
/// `RawTransactionProfilingBlock` is designed to aggregate data chunks that belong to the same transaction.
/// It keeps track of the progress and allows checking when all chunks are accumulated.
///
/// # Fields
/// - `transaction_id`: A unique identifier for the transaction.
/// - `buffer`: A vector of bytes that stores the accumulated data chunks.
/// - `target_chunks`: The total number of chunks expected to complete the data block.
/// - `accumulated_chunks`: The number of chunks added so far.
///
/// # Usage
/// Use the methods provided by `RawTransactionProfilingBlock` to manipulate its data:
/// - Create a new instance using the `new` method.
/// - Add data chunks using the `add_chunk` method.
/// - Verify if the data block is complete with the `is_complete` method.
/// - Retrieve the collected data using the `get_data` method.
///
/// # Example
/// ```ignore
///
/// let transaction_id = TransactionId::new();
/// let mut data_block = RawTransactionProfilingBlock::new(transaction_id, 3);
///
/// data_block.add_chunk(b"chunk1");
/// data_block.add_chunk(b"chunk2");
/// data_block.add_chunk(b"chunk3");
///
/// assert!(data_block.is_complete());
/// assert_eq!(data_block.get_data(), b"chunk1chunk2chunk3");
/// ```
pub struct RawTransactionProfilingBlock {
    transaction_id: TransactionId,
    buffer: Vec<u8>,
    target_chunks: usize,
    accumulated_chunks: usize,
}

impl Debug for RawTransactionProfilingBlock {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let buffer = Bytes::from(&self.buffer[..]);

        f.debug_struct("RawTransactionProfilingBlock")
            .field("transaction_id", &self.transaction_id)
            .field(
                "progress",
                &format!("{}/{}", self.accumulated_chunks, self.target_chunks),
            )
            .field("buffer", &buffer)
            .finish()
    }
}

impl RawTransactionProfilingBlock {
    pub fn new(transaction_id: TransactionId, target_chunks: usize) -> Self {
        Self {
            transaction_id,
            buffer: vec![],
            target_chunks,
            accumulated_chunks: 0,
        }
    }

    pub fn add_chunk(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        self.accumulated_chunks += 1;
    }

    pub fn is_complete(&self) -> bool {
        self.accumulated_chunks == self.target_chunks
    }

    pub fn get_data(&self) -> &[u8] {
        &self.buffer
    }
}
