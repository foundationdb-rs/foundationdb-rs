use std::string::FromUtf8Error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Unable to decode UTF-8 string from slice : {0}")]
    Utf8Decode(#[from] FromUtf8Error),
}
