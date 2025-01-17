use crate::arbitrary::errors::ToBytesError;
use crate::arbitrary::to_bytes::{ToBytes, ToBytesWithProtocolVersion};
use crate::arbitrary::writer::Writer;
use crate::events::{Event, ProfilingEvent};
use crate::protocol_version::ProtocolVersion;

mod base_info;
mod commit_info;
mod error_commit_info;
mod error_get_info;
mod error_get_range_info;
mod get_info;
mod get_range_info;
mod get_version_info;
mod key_range;
mod mutation;

impl ToBytesWithProtocolVersion for Event {
    fn to_bytes_with_protocol_version(
        &self,
        writer: &mut Writer<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<(), ToBytesError> {
        match self {
            Event::GetVersion(event) => {
                event.to_bytes_with_protocol_version(writer, protocol_version)?
            }
            Event::Get(event) => event.to_bytes(writer)?,
            Event::GetRange(event) => event.to_bytes(writer)?,
            Event::Commit(event) => {
                event.to_bytes_with_protocol_version(writer, protocol_version)?
            }
            Event::ErrorGet(event) => event.to_bytes(writer)?,
            Event::ErrorGetRange(event) => event.to_bytes(writer)?,
            Event::ErrorCommit(event) => {
                event.to_bytes_with_protocol_version(writer, protocol_version)?
            }
        }
        Ok(())
    }
}

impl ToBytesWithProtocolVersion for ProfilingEvent {
    fn to_bytes_with_protocol_version(
        &self,
        writer: &mut Writer<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<(), ToBytesError> {
        let ProfilingEvent { base_info, event } = self;

        (event.event_id() as u32).to_bytes(writer)?;
        base_info.to_bytes_with_protocol_version(writer, protocol_version)?;
        event.to_bytes_with_protocol_version(writer, protocol_version)?;
        Ok(())
    }
}
