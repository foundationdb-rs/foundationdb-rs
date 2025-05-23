//! FoundationDB natively implements transaction profiling and analyzing.
//!
//! The transactions are sampled at the specified rate, and all the events for that sampled transaction are recorded.
//! Then at the 30-second interval, the data for all the sampled transactions during that interval is flushed to the database.
//! The sampled data is written into a special key space `\xff\x02/fdbClientInfo/ - \xff\x02/fdbClientInfo0`
//!
//! [source](https://apple.github.io/foundationdb/transaction-profiler-analyzer.html)
//!
//! Each data is recorded as chunked events. Events are referenced by the tuple (VersionStamp, TransactionId);
//! then a header defined how many chunks defined the data block.
//!
//! Profiling Keys look like this:
//! ```ignore
//!     FF               - 2 bytes \xff\x02
//!     SSSSSSSSSS       - 10 bytes Version Stamp
//!     RRRRRRRRRRRRRRRR - 16 bytes Transaction id
//!     NNNN             - 4 Bytes Chunk number
//!     TTTT             - 4 Bytes Total number of chunks
//!```
//! [source](https://github.com/apple/foundationdb/blob/main/contrib/transaction_profiling_analyzer/transaction_profiling_analyzer.py#L413)
//!
//! To get the data block, each chunk that composes the data block must be accumulated until
//! reaching the "chunk number"
//!
//! Noted: It could have more than one event in the data block

use crate::events::ProfilingEvent;
use crate::parse::{Parse, ParseWithProtocolVersion};
use crate::parsed_key::parse_key;
use crate::protocol_version::ProtocolVersion;
use crate::raw_transaction_profiling_block::RawTransactionProfilingBlock;
use crate::scanner::Scanner;
use foundationdb::{FdbBindingError, RangeOption, RetryableTransaction, Transaction};
use futures_util::{Stream, TryStreamExt};
use std::pin::pin;

pub mod arbitrary;
mod errors;
pub mod events;
mod parse;
mod parsed_key;
pub mod protocol_version;
mod raw_transaction_profiling_block;
mod scanner;

/// Defined by the following [pattern](https://github.com/apple/foundationdb/blob/main/contrib/transaction_profiling_analyzer/transaction_profiling_analyzer.py#L421)
pub const PROFILE_PREFIX: &[u8; 32] = b"\xff\x02/fdbClientInfo/client_latency/";

/// Retrieves raw profiling datablocks stored in the database within the specified read version range.
///
/// This function fetches raw events from the FoundationDB range key-value store. The keys are
/// processed to parse the profiling event data. Events are grouped into chunks and consolidated
/// into complete [RawTransactionProfilingBlock] instances. The resulting stream yields these complete profiling event
/// data blocks. Events are referenced using a key structure which includes the transaction ID,
/// version stamp, and a chunk identifier.
///
/// # Arguments
///
/// * `trx` - A reference to the FoundationDB `Transaction` used for fetching data.
/// * `start_read_version` - An optional lower bound read version. If provided, only events
///                          occurring at or after this version are fetched.
/// * `end_read_version` - An optional upper bound read version. If provided, only events
///                        occurring before this version are fetched.
///
/// # Returns
///
/// Returns an asynchronous stream (`impl Stream`) where each item of the stream is either:
/// * A `Result<DataBlock, FdbBindingError>`
///   - `DataBlock`: A complete profiling event data block.
///   - `FdbBindingError`: An error that occurred during data retrieval or processing.
///
/// # Errors
///
/// This function will return an error in the following cases:
/// * If the transaction (provided by `trx`) encounters issues fetching ranges.
/// * If an error occurs during key or value parsing.
///
/// # Notes
///
/// Profiling keys follow a specific binary layout defined by the FoundationDB profiling data space:
/// * `\xff\x02/fdbClientInfo/client_latency/` acts as the prefix for profiling events.
/// * Events are referred to by tuple: (VersionStamp, TransactionId).
/// * Events contain chunks, defined by a chunk number and total chunks.
///
/// The function ensures fully constructed events are yielded only once all their chunks are retrieved.
///
/// # Example
///
/// ```no_run
/// use foundationdb::{Database, Transaction};
/// use futures_util::StreamExt;
/// use foundationdb_profiling::get_raw_datablocks;
///
/// async fn example_usage(trx: &Transaction) {
///     let mut stream = get_raw_datablocks(trx, Some(1), Some(10));
///     while let Some(event) = stream.next().await {
///         match event {
///             Ok(data_block) => {
///                 println!("Received a data block: {:?}", data_block);
///             }
///             Err(e) => {
///                 eprintln!("Error: {:?}", e);
///             }
///         }
///     }
/// }
/// ```
pub async fn get_raw_datablocks(
    trx: &Transaction,
    start_read_version: Option<u64>,
    end_read_version: Option<u64>,
) -> impl Stream<Item = Result<RawTransactionProfilingBlock, FdbBindingError>> + use<'_> {
    // build the start prefix of the range
    let start_key = if let Some(start_version) = start_read_version {
        let mut key = PROFILE_PREFIX.to_vec();
        key.extend_from_slice(&start_version.to_be_bytes());
        // the start_version is a read version on 8 bytes
        // to build the VersionStamp the batch version must be added
        // as we don't know it, we append 2 0x0 bytes
        // https://github.com/apple/foundationdb/blob/main/contrib/transaction_profiling_analyzer/transaction_profiling_analyzer.py#L463
        key.extend_from_slice(b"\x00\x00");
        key
    } else {
        // If there is no start_version, then the scan begins
        // at the beginning of the Profiling Keyspace
        PROFILE_PREFIX.to_vec()
    };

    let end_key = if let Some(end_version) = end_read_version {
        let mut key = PROFILE_PREFIX.to_vec();
        key.extend_from_slice(&end_version.to_be_bytes());
        key.extend_from_slice(b"\x00\x00");
        key
    } else {
        // If there is no end_version, then the scan ends
        // at the very last key of the Profiling Keyspace
        let mut key = PROFILE_PREFIX.to_vec();
        key.extend_from_slice(b"\xff");
        key
    };

    // Create an async iterator which yields completed DataBlocks
    async_stream::try_stream! {
        // Get an iterator over the range inside the Profiling Keyspace
        let profiling_events_raw_key_value_stream = trx
        .get_ranges_keyvalues(RangeOption::from((start_key, end_key)), true);

        // stream must be pinned to be moveable out of the function
        let mut profiling_events_raw_key_value_stream = pin!(profiling_events_raw_key_value_stream);

        // Keeps track of the actual Datablock in progress
        let mut in_progress_datablock = None;

        loop {
            // Get a key/value
            let raw_key_value = profiling_events_raw_key_value_stream
                .try_next()
                .await
                .map_err(FdbBindingError::from)?;

            // Take a decision on raw data
            match raw_key_value {
                // end of the range, no more raw keys
                None => {
                    break
                }
                // available raw key
                Some(data) => {
                    // Try to parse the raw data as a ParsedKey
                    let parsed_key = parse_key(data.key())
                        .await
                        .map_err(FdbBindingError::new_custom_error)?;

                    // Take decision on what doing with the ParsedKey
                    match in_progress_datablock {
                        // No DataBlock in progress
                        None => {
                            // Create a new Datablock from the current ParsedKey
                            let mut datablock = RawTransactionProfilingBlock::new(parsed_key.transaction_id, parsed_key.total_chunk);
                            // Accumulate the first data chunk
                            datablock.add_chunk(data.value());
                            // If the DataBlock is whole
                            if datablock.is_complete() {
                                // yield a DataBlock
                                yield datablock
                            } else {
                                // Set the DataBlock as new in progress to
                                // accumulate more data chunks
                                in_progress_datablock = Some(datablock)
                            }

                        }
                        // If DataBlock is progressing
                        Some(ref mut event) => {
                            // Add the new data chunk
                            event.add_chunk(data.value());
                            // Yield DataBlock if complete
                            if event.is_complete() {
                                // 'take()' reset the `in_progress_datablock` state
                                if let Some(event) = in_progress_datablock.take() {
                                    yield event
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Retrieves a stream of `ProfilingEvent` from a given `RetryableTransaction`.
///
/// `start_version` and `end_version` are used to filter the events to retrieve.
/// If `start_version` is `Some(version)`, only events with a version greater than `version` are retrieved.
/// If `end_version` is `Some(version)`, only events with version less or equal to `version` are retrieved.
///
/// The stream is ordered by increasing version.
///
/// If any error occurs during the retrieval, the `Stream` will yield an ` FdbBindingError `.
///
/// # Errors
///
/// * `FdbBindingError` if any error occurs during the retrieval.
pub async fn get_events(
    trx: RetryableTransaction,
    start_version: Option<u64>,
    end_version: Option<u64>,
) -> impl Stream<Item = Result<ProfilingEvent, FdbBindingError>> {
    async_stream::try_stream! {
        let raw_events = get_raw_datablocks(&trx, start_version, end_version).await;
        let mut raw_events = pin!(raw_events);
        while let Some(raw_event) = raw_events.try_next().await? {

            let data = raw_event.get_data();
                let mut scanner = Scanner::new(data);
                let protocol_version = &ProtocolVersion::parse(&mut scanner)
                    .await.map_err( FdbBindingError::new_custom_error)?;

            while !scanner.remaining().is_empty() {
                yield ProfilingEvent::parse_with_protocol_version(&mut scanner, protocol_version).await.map_err( FdbBindingError::new_custom_error)?;
            }

        }
    }
}
