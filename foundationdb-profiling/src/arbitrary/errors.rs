use std::string::FromUtf8Error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToBytesError {
    #[error("Unable to encode UTF-8 string : {0}")]
    Utf8Encode(#[from] FromUtf8Error),
    #[error("IO error occurred : {0}")]
    Io(#[from] std::io::Error),
}
