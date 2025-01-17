use std::io::Cursor;
use std::ops::{Deref, DerefMut};

pub struct Writer<'a> {
    cursor: Cursor<&'a mut [u8]>,
}

impl<'a> Writer<'a> {
    pub fn new(data: &'a mut [u8]) -> Self {
        Writer {
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
