use foundationdb_profiling::parse::Parse;
use futures_util::io::Cursor;
use std::error::Error;
use std::ops::{Deref, DerefMut};

pub struct Scanner<'a> {
    cursor: Cursor<&'a [u8]>,
}

impl<'a> Scanner<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Scanner {
            cursor: Cursor::new(data),
        }
    }

    pub fn bump_by(&mut self, size: usize) {
        self.cursor
            .set_position(self.cursor.position() + size as u64)
    }

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
