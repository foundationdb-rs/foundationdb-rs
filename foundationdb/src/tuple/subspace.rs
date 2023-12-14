// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use super::*;
use super::pack::PackedTuple;
use crate::{KeySelector, RangeOption, Transaction};
use std::borrow::Cow;
use std::hash::Hash;

/// Represents a well-defined region of keyspace in a FoundationDB database
///
/// It provides a convenient way to use FoundationDB tuples to define namespaces for
/// different categories of data. The namespace is specified by a prefix tuple which is prepended
/// to all tuples packed by the subspace. When unpacking a key with the subspace, the prefix tuple
/// will be removed from the result.
///
/// As a best practice, API clients should use at least one subspace for application data. For
/// general guidance on subspace usage, see the Subspaces section of the [Developer Guide].
///
/// [Developer Guide]: https://apple.github.io/foundationdb/developer-guide.html#subspaces
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Subspace {
    prefix: PackedTuple<Vec<u8>>,
}

impl<E: TuplePack> From<E> for Subspace {
    fn from(e: E) -> Self {
        let mut prefix = PackedTuple::new(Vec::new());
        e.pack_root(&mut prefix).expect("failed to pack");
        Self {
            prefix,
        }
    }
}

impl Subspace {
    /// `all` returns the Subspace corresponding to all keys in a FoundationDB database.
    pub fn all() -> Self {
        Self {
            prefix: PackedTuple::new(Vec::new()),
        }
    }

    /// Returns a new Subspace from the provided bytes. We assume that the provided bytes
    /// don't contain any incomplete versionstamp, since they don't carry that information.
    pub fn from_bytes(prefix: impl Into<Vec<u8>>) -> Self {
        let prefix = PackedTuple::new(prefix.into());
        Self {
            prefix,
        }
    }

    /// Convert into packed bytes
    pub fn packed(&self) -> Vec<u8> {
        self.prefix.clone().pack().expect("failed to pack prefix")
    }

    /// Get raw prefix bytes
    pub fn bytes(&self) -> &[u8] {
        self.prefix.as_bytes()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.prefix.inner()
    }

    /// Returns a new Subspace whose prefix extends this Subspace with a given tuple encodable.
    pub fn subspace<T: TuplePack>(&self, t: &T) -> Self {
        let mut prefix = self.prefix.clone();
        t.pack_root(&mut prefix).expect("failed to pack");
        Self {
            prefix,
        }
    }

    /// Returns the key encoding the specified Tuple with the prefix of this Subspace
    /// prepended.
    pub fn pack<T: TuplePack>(&self, t: &T) -> Vec<u8> {
        let mut out = self.prefix.clone();
        t.pack_root(&mut out).expect("failed to pack");
        out.pack().expect("failed to pack")
    }

    /// `unpack` returns the Tuple encoded by the given key with the prefix of this Subspace
    /// removed.  `unpack` will return an error if the key is not in this Subspace or does not
    /// encode a well-formed Tuple.
    pub fn unpack<'de, T: TupleUnpack<'de>>(&self, key: &'de [u8]) -> PackResult<T> {
        if !self.is_start_of(key) {
            return Err(PackError::BadPrefix);
        }
        let key = &key[self.prefix.as_bytes().len()..];
        unpack(key)
    }

    /// `is_start_of` returns true if the provided key starts with the prefix of this Subspace,
    /// indicating that the Subspace logically contains the key.
    pub fn is_start_of(&self, key: &[u8]) -> bool {
        key.starts_with(self.prefix.as_bytes())
    }

    /// `range` returns first and last key of given Subspace
    pub fn range(&self) -> (Vec<u8>, Vec<u8>) {
        let prefix_bytes = self.prefix.as_bytes();
        let mut begin = Vec::with_capacity(prefix_bytes.len() + 1);
        begin.extend_from_slice(prefix_bytes);
        begin.push(0x00);

        let mut end = Vec::with_capacity(prefix_bytes.len() + 1);
        end.extend_from_slice(prefix_bytes);
        end.push(0xff);

        (begin, end)
    }
}

impl<'a> From<&'a Subspace> for RangeOption<'static> {
    fn from(subspace: &Subspace) -> Self {
        let (begin, end) = subspace.range();

        Self {
            begin: KeySelector::first_greater_or_equal(Cow::Owned(begin)),
            end: KeySelector::first_greater_or_equal(Cow::Owned(end)),
            ..Self::default()
        }
    }
}

impl Transaction {
    pub fn clear_subspace_range(&self, subspace: &Subspace) {
        let (begin, end) = subspace.range();
        self.clear_range(&begin, &end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn sub() {
        let ss0: Subspace = 1.into();
        let ss1 = ss0.subspace(&2);

        let ss2: Subspace = (1, 2).into();

        assert_eq!(ss1.bytes(), ss2.bytes());
    }

    #[test]
    fn pack_unpack() {
        let ss0: Subspace = 1.into();
        let tup = (2, 3);

        let packed = ss0.pack(&tup);
        let expected = pack(&(1, 2, 3));
        assert_eq!(expected, packed);

        let tup_unpack: (i64, i64) = ss0.unpack(&packed).unwrap();
        assert_eq!(tup, tup_unpack);

        assert!(ss0.unpack::<(i64, i64, i64)>(&packed).is_err());
    }

    #[test]
    fn subspace_pack_with_versionstamp() {
        let subspace: Subspace = 1.into();
        let tup = (Versionstamp::incomplete(0), 2);

        let packed = subspace.pack(&tup);
        let expected = pack_with_versionstamp(&(1, Versionstamp::incomplete(0), 2));
        assert_eq!(expected, packed);
    }

    #[test]
    fn subspace_unpack_with_versionstamp() {
        // On unpack, the versionstamp will be complete, so pack_with_versionstamp won't append
        // the offset.
        let subspace: Subspace = 1.into();
        let tup = (Versionstamp::complete([1; 10], 0), 2);
        let packed = subspace.pack(&tup);
        let tup_unpack: (Versionstamp, i64) = subspace.unpack(&packed).unwrap();
        assert_eq!(tup, tup_unpack);

        assert!(subspace
            .unpack::<(i64, Versionstamp, i64)>(&packed)
            .is_err());
    }

    #[test]
    fn unpack_with_subspace_versionstamp() {
        let subspace: Subspace = Versionstamp::complete([1; 10], 2).into();
        let tup = (Versionstamp::complete([1; 10], 0), 2);
        let packed = subspace.pack(&tup);
        let tup_unpack: (Versionstamp, i64) = subspace.unpack(&packed).unwrap();
        assert_eq!(tup, tup_unpack);
        assert!(subspace
            .unpack::<(Versionstamp, Versionstamp, i64)>(&packed)
            .is_err());
    }

    #[test]
    fn subspace_can_use_incomplete_versionstamp() {
        let subspace: Subspace = Versionstamp::incomplete(0).into();
        let tup = (1, 2);

        let packed = subspace.pack(&tup);
        let expected = pack_with_versionstamp(&(Versionstamp::incomplete(0), 1, 2));
        assert_eq!(expected, packed);
    }

    #[test]
    fn child_subspace_can_use_incomplete_versionstamp() {
        let subspace: Subspace = 1.into();
        let subspace = subspace.subspace(&Versionstamp::incomplete(0));
        let tup = (1, 2);
        let packed = subspace.pack(&tup);
        let expected = pack_with_versionstamp(&(1, Versionstamp::incomplete(0), 1, 2));
        assert_eq!(expected, packed);
    }

    #[test]
    #[should_panic]
    fn subspace_cannot_use_multiple_incomplete_versionstamps() {
        let subspace: Subspace = Versionstamp::incomplete(0).into();
        let subspace = subspace.subspace(&Versionstamp::incomplete(0));
        subspace.pack(&1);
    }

    #[test]
    #[should_panic]
    fn subspace_cannot_pack_multiple_incomplete_versionstamps() {
        let subspace: Subspace = Versionstamp::incomplete(0).into();
        subspace.pack(&Versionstamp::incomplete(0));
    }

    #[test]
    fn is_start_of() {
        let ss0: Subspace = 1.into();
        let ss1: Subspace = 2.into();
        let tup = (2, 3);

        assert!(ss0.is_start_of(&ss0.pack(&tup)));
        assert!(!ss1.is_start_of(&ss0.pack(&tup)));
        assert!(Subspace::from("start").is_start_of(&pack(&"start")));
        assert!(Subspace::from("start").is_start_of(&pack(&"start".to_string())));
        assert!(!Subspace::from("start").is_start_of(&pack(&"starting")));
        assert!(Subspace::from("start").is_start_of(&pack(&("start", "end"))));
        assert!(Subspace::from(("start", 42)).is_start_of(&pack(&("start", 42, "end"))));
    }

    #[test]
    fn range() {
        let ss: Subspace = 1.into();
        let tup = (2, 3);
        let packed = ss.pack(&tup);

        let (begin, end) = ss.range();
        assert!(packed >= begin && packed <= end);
    }

    #[test]
    fn equality() {
        let sub1 = Subspace::all().subspace(&"test");
        let sub2 = Subspace::all().subspace(&"test");
        let sub3 = Subspace::all().subspace(&"test2");
        assert_eq!(sub1, sub2);
        assert_ne!(sub1, sub3);
    }

    #[test]
    fn hash() {
        let sub1 = Subspace::all().subspace(&"test");
        let sub2 = Subspace::all().subspace(&"test2");

        let map: HashMap<Subspace, u8> = HashMap::from([(sub1, 1), (sub2, 2)]);

        assert_eq!(map.get(&Subspace::all().subspace(&"test")).unwrap(), &1);
        assert_eq!(map.get(&Subspace::all().subspace(&"test2")).unwrap(), &2);
    }
}
