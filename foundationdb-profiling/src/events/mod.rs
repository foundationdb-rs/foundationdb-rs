use crate::errors::ParseError;
use crate::parse::{Parse, ParseWithProtocolVersion};
use crate::protocol_version::ProtocolVersion;
use crate::scanner::Scanner;
#[cfg(feature = "to_bytes")]
use arbitrary::Arbitrary;
use std::error::Error;

mod base_info;
pub use base_info::BaseInfo;
mod commit_info;
pub use commit_info::CommitInfo;
mod error_commit_info;
pub use error_commit_info::ErrorCommitInfo;
mod error_get_info;
pub use error_get_info::ErrorGetInfo;
mod error_get_range_info;
pub use error_get_range_info::ErrorGetRangeInfo;
mod get_info;
pub use get_info::GetInfo;
mod get_range_info;
pub use get_range_info::GetRangeInfo;

mod get_version_info;
pub use get_version_info::GetVersionInfo;
mod key_range;
pub use key_range::KeyRange;
mod mutation;
pub use mutation::{Mutation, MutationType};

#[derive(Debug)]
#[repr(u32)]
pub enum EventId {
    GetVersion = 0,
    Get,
    GetRange,
    Commit,
    ErrorGet,
    ErrorGetRange,
    ErrorCommit,
}

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub enum Event {
    GetVersion(GetVersionInfo),
    Get(GetInfo),
    GetRange(GetRangeInfo),
    Commit(CommitInfo),
    ErrorGet(ErrorGetInfo),
    ErrorGetRange(ErrorGetRangeInfo),
    ErrorCommit(ErrorCommitInfo),
}

impl Event {
    pub fn event_id(&self) -> EventId {
        match self {
            Event::GetVersion(_) => EventId::GetVersion,
            Event::Get(_) => EventId::Get,
            Event::GetRange(_) => EventId::GetRange,
            Event::Commit(_) => EventId::Commit,
            Event::ErrorGet(_) => EventId::ErrorGet,
            Event::ErrorGetRange(_) => EventId::ErrorGetRange,
            Event::ErrorCommit(_) => EventId::ErrorCommit,
        }
    }
}

#[async_trait::async_trait]
impl ParseWithProtocolVersion for ProfilingEvent {
    /// Parses a `ProfilingEvent` from the given `Scanner` and `protocol_version`.
    ///
    /// The `ProfilingEvent` is a container for a profiling event, which consists of a `BaseInfo` and an `Event`.
    ///
    /// # Errors
    ///
    /// If the underlying `Scanner` fails to parse the expected fields, or if
    /// the `protocol_version` is invalid, an error is returned.
    async fn parse_with_protocol_version(
        scanner: &mut Scanner<'_>,
        protocol_version: &ProtocolVersion,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let event_id = EventId::try_from(u32::parse(scanner).await?)?;
        let base_info = BaseInfo::parse_with_protocol_version(scanner, protocol_version).await?;
        let event = match event_id {
            EventId::GetVersion => {
                GetVersionInfo::parse_with_protocol_version(scanner, protocol_version)
                    .await
                    .map(Event::GetVersion)?
            }
            EventId::Get => GetInfo::parse_with_protocol_version(scanner, protocol_version)
                .await
                .map(Event::Get)?,
            EventId::GetRange => {
                GetRangeInfo::parse_with_protocol_version(scanner, protocol_version)
                    .await
                    .map(Event::GetRange)?
            }
            EventId::Commit => CommitInfo::parse_with_protocol_version(scanner, protocol_version)
                .await
                .map(Event::Commit)?,
            EventId::ErrorGet => Event::ErrorGet(
                ErrorGetInfo::parse_with_protocol_version(scanner, protocol_version).await?,
            ),
            EventId::ErrorGetRange => Event::ErrorGetRange(
                ErrorGetRangeInfo::parse_with_protocol_version(scanner, protocol_version).await?,
            ),
            EventId::ErrorCommit => Event::ErrorCommit(
                ErrorCommitInfo::parse_with_protocol_version(scanner, protocol_version).await?,
            ),
        };

        Ok(ProfilingEvent { base_info, event })
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "to_bytes", derive(Arbitrary))]
pub struct ProfilingEvent {
    pub(crate) base_info: BaseInfo,
    pub(crate) event: Event,
}

impl TryFrom<u32> for EventId {
    type Error = ParseError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::GetVersion),
            1 => Ok(Self::Get),
            2 => Ok(Self::GetRange),
            3 => Ok(Self::Commit),
            4 => Ok(Self::ErrorGet),
            5 => Ok(Self::ErrorGetRange),
            6 => Ok(Self::ErrorCommit),
            _ => Err(ParseError::UnknownEventId(value)),
        }
    }
}

/// Parses a `T` event from the given `Scanner` if `actual_version` is
/// greater than `target_version`. Otherwise, returns `None`.
///
/// The `Scanner` is expected to be positioned at the start of the
/// optional `T` event. If the `actual_version` is greater than
/// `target_version`, the `Scanner` is expected to be positioned at a
/// boolean indicating whether the `T` event is present. If the boolean
/// is `true`, the `Scanner` is expected to be positioned at the `T`
/// event. If the boolean is `false`, the `Scanner` is expected to be
/// positioned at the end of the optional `T` event and `None` is
/// returned.
///
/// # Errors
///
/// If the underlying `Scanner` fails to parse the expected fields, or
/// if the `actual_version` is invalid, an error is returned.
async fn parse_protocol_if_greater_than_with_flag<T: Parse>(
    scanner: &mut Scanner<'_>,
    actual_version: &ProtocolVersion,
    target_version: &ProtocolVersion,
) -> Result<Option<T>, Box<dyn Error + Send + Sync>> {
    if actual_version > target_version {
        if bool::parse(scanner).await? {
            Ok(Some(scanner.parse().await?))
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}

/// Parses a `T` event from the given `Scanner` if `actual_version` is
/// greater than `target_version`. Otherwise, returns `None`.
///
/// The `Scanner` is expected to be positioned at the start of the
/// optional `T` event. If the `actual_version` is greater than
/// `target_version`, the `Scanner` is expected to be positioned at the
/// `T` event. If the `actual_version` is not greater than
/// `target_version`, the `Scanner` is expected to be positioned at the
/// end of the optional `T` event and `None` is returned.
///
/// # Errors
///
/// If the underlying `Scanner` fails to parse the expected fields, or
/// if the `actual_version` is invalid, an error is returned.
async fn parse_protocol_if_greater_than<T: Parse>(
    scanner: &mut Scanner<'_>,
    actual_version: &ProtocolVersion,
    target_version: &ProtocolVersion,
) -> Result<Option<T>, Box<dyn Error + Send + Sync>> {
    if actual_version > target_version {
        scanner.parse().await
    } else {
        Ok(None)
    }
}
