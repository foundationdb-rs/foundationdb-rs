use crate::parse::Parse;
use futures_util::io::Cursor;
use std::error::Error;
use std::ops::{Deref, DerefMut};

/// A `Scanner` provides an abstraction over a cursor for scanning through a byte slice.
///
/// # Example
///
/// ```ignore
/// use crate::Scanner;
///
/// let data = b"Hello, world!";
/// let mut scanner = Scanner::new(data);
///
/// // Access remaining bytes
/// assert_eq!(scanner.remaining(), b"Hello, world!");
///
/// // Advance the cursor by 7 bytes
/// scanner.bump_by(7);
///
/// // Remaining bytes after advancing
/// assert_eq!(scanner.remaining(), b"world!");
/// ```
pub(crate) struct Scanner<'a> {
    cursor: Cursor<&'a [u8]>,
}

impl<'a> Scanner<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Scanner {
            cursor: Cursor::new(data),
        }
    }

    /// Advances the cursor by a specified number of bytes.
    ///
    /// # Arguments
    ///
    /// * `size` - The number of bytes to advance the cursor.
    pub fn bump_by(&mut self, size: usize) {
        self.cursor
            .set_position(self.cursor.position() + size as u64)
    }

    /// Returns the remaining bytes in the cursor.
    ///
    /// This method provides a slice of the byte array starting at the
    /// current cursor position and continuing to the end of the data.
    ///
    /// # Returns
    ///
    /// A slice containing the remaining bytes from the cursor position onward.
    pub fn remaining(&self) -> &[u8] {
        &self.cursor.get_ref()[(self.cursor.position() as usize)..]
    }
}

impl<'a> Deref for Scanner<'a> {
    type Target = Cursor<&'a [u8]>;

    fn deref(&self) -> &Self::Target {
        &self.cursor
    }
}
/// This implementation provides a blanket `Deref` trait for the `Scanner` struct,
/// allowing seamless access to its inner `Cursor<&[u8]>` methods.
///
/// By implementing `Deref`, all methods available on the `Cursor` type
/// can be accessed directly via the `Scanner` instance. This removes
/// the need to explicitly reference the `cursor` field, streamlining the API usage.
///
/// # Example
///
/// ```ignore
/// use crate::Scanner;
/// use std::io::Seek;
///
/// let data = b"Example seamless access";
/// let scanner = Scanner::new(data);
///
/// // Accessing a Cursor method directly via the Scanner instance
/// assert_eq!(scanner.position(), 0);
/// ```
impl<'a> DerefMut for Scanner<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cursor
    }
}

impl<'a> Scanner<'a> {
    pub async fn parse<T: Parse>(&mut self) -> Result<T, Box<dyn Error + Send + Sync>> {
        T::parse(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_scanner() {
        let data = b"Hello, world!";
        let mut scanner = Scanner::new(data);
        assert_eq!(scanner.remaining(), b"Hello, world!");
        scanner.bump_by(7);
        assert_eq!(scanner.remaining(), b"world!");
    }
}
