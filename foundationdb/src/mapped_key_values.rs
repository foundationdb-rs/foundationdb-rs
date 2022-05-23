// Copyright 2022 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

/// Definitions of MappedKeyValues, used in api version 710 and more.

use crate::error;
use crate::future::{FdbFutureHandle, FdbKeyValue};
use crate::{FdbError, FdbResult};
use foundationdb_sys as fdb_sys;
use std::fmt;

use std::ops::Deref;
use std::sync::Arc;

/// An slice of keyvalues owned by a foundationDB future produced by the `get_mapped` method.
pub struct MappedKeyValues {
    _f: FdbFutureHandle,
    mapped_keyvalues: *const fdb_sys::FDBMappedKeyValue,
    len: i32,
    more: bool,
}
unsafe impl Sync for MappedKeyValues {}
unsafe impl Send for MappedKeyValues {}

impl MappedKeyValues {
    /// `true` if there is another range after this one
    pub fn more(&self) -> bool {
        self.more
    }
}

impl TryFrom<FdbFutureHandle> for MappedKeyValues {
    type Error = FdbError;
    fn try_from(f: FdbFutureHandle) -> FdbResult<Self> {
        let mut keyvalues = std::ptr::null();
        let mut len = 0;
        let mut more = 0;

        unsafe {
            error::eval(fdb_sys::fdb_future_get_mappedkeyvalue_array(
                f.as_ptr(),
                &mut keyvalues,
                &mut len,
                &mut more,
            ))?
        }

        Ok(MappedKeyValues {
            _f: f,
            mapped_keyvalues: keyvalues,
            len,
            more: more != 0,
        })
    }
}

#[repr(transparent)]
pub struct FdbMappedKeyValue(fdb_sys::FDBMappedKeyValue);

impl PartialEq for FdbMappedKeyValue {
    fn eq(&self, other: &Self) -> bool {
        (self.parent_key(), self.parent_value()) == (other.parent_key(), other.parent_value())
    }
}
impl Eq for FdbMappedKeyValue {}
impl fmt::Debug for FdbMappedKeyValue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "({:?}, {:?})",
            crate::tuple::Bytes::from(self.parent_key()),
            crate::tuple::Bytes::from(self.parent_value())
        )
    }
}

impl FdbMappedKeyValue {
    pub fn parent_key(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(self.0.key.key as *const u8, self.0.key.key_length as usize)
        }
    }

    pub fn parent_value(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.0.value.key as *const u8,
                self.0.value.key_length as usize,
            )
        }
    }

    pub fn begin_range(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.0.getRange.begin.key.key as *const u8,
                self.0.getRange.begin.key.key_length as usize,
            )
        }
    }

    pub fn end_range(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.0.getRange.end.key.key as *const u8,
                self.0.getRange.end.key.key_length as usize,
            )
        }
    }

    pub fn key_values(&self) -> &[FdbKeyValue] {
        unsafe {
            &*(std::slice::from_raw_parts(self.0.getRange.data, self.0.getRange.m_size as usize)
                as *const [fdb_sys::FDBKeyValue] as *const [FdbKeyValue])
        }
    }
}

impl Deref for MappedKeyValues {
    type Target = [FdbMappedKeyValue];

    fn deref(&self) -> &Self::Target {
        assert_eq_size!(FdbMappedKeyValue, fdb_sys::FDBMappedKeyValue);
        assert_eq_align!(FdbMappedKeyValue, fdb_sys::FDBMappedKeyValue);
        unsafe {
            &*(std::slice::from_raw_parts(self.mapped_keyvalues, self.len as usize)
                as *const [fdb_sys::FDBMappedKeyValue]
                as *const [FdbMappedKeyValue])
        }
    }
}

impl AsRef<[FdbMappedKeyValue]> for MappedKeyValues {
    fn as_ref(&self) -> &[FdbMappedKeyValue] {
        self.deref()
    }
}

impl<'a> IntoIterator for &'a MappedKeyValues {
    type Item = &'a FdbMappedKeyValue;
    type IntoIter = std::slice::Iter<'a, FdbMappedKeyValue>;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().iter()
    }
}

impl IntoIterator for MappedKeyValues {
    type Item = FdbMappedValue;
    type IntoIter = FdbMappedValuesIter;

    fn into_iter(self) -> Self::IntoIter {
        FdbMappedValuesIter {
            f: Arc::new(self._f),
            keyvalues: self.mapped_keyvalues,
            len: self.len,
            pos: 0,
        }
    }
}

unsafe impl Send for FdbMappedValue {}

impl Deref for FdbMappedValue {
    type Target = FdbMappedKeyValue;
    fn deref(&self) -> &Self::Target {
        assert_eq_size!(FdbMappedKeyValue, fdb_sys::FDBMappedKeyValue);
        assert_eq_align!(FdbMappedKeyValue, fdb_sys::FDBMappedKeyValue);
        unsafe { &*(self.mapped_keyvalue as *const FdbMappedKeyValue) }
    }
}
impl AsRef<FdbMappedKeyValue> for FdbMappedValue {
    fn as_ref(&self) -> &FdbMappedKeyValue {
        self.deref()
    }
}
impl PartialEq for FdbMappedValue {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}
impl Eq for FdbMappedValue {}

pub struct FdbMappedValue {
    _f: Arc<FdbFutureHandle>,
    mapped_keyvalue: *const fdb_sys::FDBMappedKeyValue,
}

/// An iterator of keyvalues owned by a foundationDB future
pub struct FdbMappedValuesIter {
    f: Arc<FdbFutureHandle>,
    keyvalues: *const fdb_sys::FDBMappedKeyValue,
    len: i32,
    pos: i32,
}

unsafe impl Send for FdbMappedValuesIter {}

impl Iterator for FdbMappedValuesIter {
    type Item = FdbMappedValue;
    fn next(&mut self) -> Option<Self::Item> {
        #[allow(clippy::iter_nth_zero)]
        self.nth(0)
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        let pos = (self.pos as usize).checked_add(n);
        match pos {
            Some(pos) if pos < self.len as usize => {
                // safe because pos < self.len
                let keyvalue = unsafe { self.keyvalues.add(pos) };
                self.pos = pos as i32 + 1;

                Some(FdbMappedValue {
                    _f: self.f.clone(),
                    mapped_keyvalue: keyvalue,
                })
            }
            _ => {
                self.pos = self.len;
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = (self.len - self.pos) as usize;
        (rem, Some(rem))
    }
}
impl ExactSizeIterator for FdbMappedValuesIter {
    #[inline]
    fn len(&self) -> usize {
        (self.len - self.pos) as usize
    }
}
impl DoubleEndedIterator for FdbMappedValuesIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.nth_back(0)
    }

    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        if n < self.len() {
            self.len -= 1 + n as i32;
            // safe because len < original len
            let keyvalue = unsafe { self.keyvalues.add(self.len as usize) };
            Some(FdbMappedValue {
                _f: self.f.clone(),
                mapped_keyvalue: keyvalue,
            })
        } else {
            self.pos = self.len;
            None
        }
    }
}
