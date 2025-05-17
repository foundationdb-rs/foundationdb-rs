use std::io::Cursor;
use std::ops::{Deref, DerefMut};

/// A writer that can be used to write bytes into a buffer
pub struct Writer<'a> {
    cursor: Cursor<&'a mut [u8]>,
}

impl<'a> Writer<'a> {
    /// Create a new `Writer` that writes into `data`.
    ///
    /// `data` is a mutable reference to the byte slice that this writer will write into.
    pub fn new(data: &'a mut [u8]) -> Self {
        Writer {
            cursor: Cursor::new(data),
        }
    }

    /// Advances the writer's internal cursor by `size` bytes.
    ///
    /// This method does not write any data to the underlying buffer, but instead
    /// just moves the position of the internal cursor. This is useful in
    /// situations where you want to write a value at a specific offset without
    /// having to pad the buffer with zeroes, or when you want to write a value
    /// that is not representable as a `u8`.
    pub fn bump_by(&mut self, size: usize) {
        self.cursor
            .set_position(self.cursor.position() + size as u64)
    }

    /// Returns a slice of the remaining bytes in the buffer.
    ///
    /// The slice starts at the current position of the internal cursor and
    /// continues to the end of the buffer.
    pub fn remaining(&self) -> &[u8] {
        &self.cursor.get_ref()[(self.cursor.position() as usize)..]
    }

    /// Consumes the `Writer` and returns the underlying `Cursor` that it was writing into.
    ///
    /// This is useful when you want to return a `Cursor` from a function, or if you want to
    /// write into another buffer later on.
    pub fn into_inner(self) -> Cursor<&'a mut [u8]> {
        self.cursor
    }
}

impl<'a> Deref for Writer<'a> {
    type Target = Cursor<&'a mut [u8]>;

    fn deref(&self) -> &Self::Target {
        &self.cursor
    }
}

impl<'a> DerefMut for Writer<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cursor
    }
}

// impl<'a> Writer<'a> {
//     pub(crate) async fn parse<T: Parse>(&mut self) -> Result<T, Box<dyn Error + Send + Sync>> {
//         T::parse(self).await
//     }
// }
