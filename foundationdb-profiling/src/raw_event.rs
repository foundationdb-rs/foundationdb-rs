use crate::parsed_key::TransactionId;
use foundationdb_tuple::Bytes;
use std::fmt::{Debug, Formatter};

pub struct RawEvent {
    transaction_id: TransactionId,
    buffer: Vec<u8>,
    target_chunks: usize,
    accumulated_chunks: usize,
}

impl Debug for RawEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let buffer = Bytes::from(&self.buffer[..]);

        f.debug_struct("RawEvent")
            .field("transaction_id", &self.transaction_id)
            .field(
                "progress",
                &format!("{}/{}", self.accumulated_chunks, self.target_chunks),
            )
            .field("buffer", &buffer)
            .finish()
    }
}

impl RawEvent {
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
