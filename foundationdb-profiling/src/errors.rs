use std::string::FromUtf8Error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Unsupported protocol version raw value : {0:#0x}")]
    UnsupportedProtocolVersion(u64),
    #[error("Unknow event id : {0}")]
    UnknownEventId(u32),
    #[error("Unknown mutation type : {0}")]
    UnknownMutationType(u8),
    #[error("Unable to decode UTF-8 string from slice : {0}")]
    Utf8Decode(#[from] FromUtf8Error),
    #[error("Expecting double value")]
    ExepectingDouble,
}
