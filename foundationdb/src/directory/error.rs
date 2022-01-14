// Copyright 2018 foundationdb-rs developers, https://github.com/Clikengo/foundationdb-rs/graphs/contributors
// Copyright 2013-2018 Apple, Inc and the FoundationDB project authors.
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Errors that can be thrown by Directory.

use crate::error;
use crate::tuple::hca::HcaError;
use crate::tuple::PackError;
use std::io;

/// The enumeration holding all possible errors from a Directory.
#[derive(Debug)]
pub enum DirectoryError {
    /// cannot modify the root directory
    CannotModifyRootDirectory,
    /// prefix is already used
    DirectoryPrefixInUse,
    /// Directory does not exists
    DirectoryDoesNotExists,
    /// missing path.
    NoPathProvided,
    /// tried to create an already existing path.
    DirAlreadyExists,
    /// missing directory.
    PathDoesNotExists,
    /// Parent does not exists
    ParentDirDoesNotExists,
    /// the layer is incompatible.
    IncompatibleLayer,
    /// the destination directory cannot be a subdirectory of the source directory.
    BadDestinationDirectory,
    /// Bad directory version.
    Version(String),
    /// cannot specify a prefix unless manual prefixes are enabled
    PrefixNotAllowed,
    /// cannot specify a prefix in a partition.
    CannotPrefixInPartition,
    /// the root directory cannot be moved
    CannotMoveRootDirectory,
    CannotMoveBetweenPartition,
    /// the destination directory cannot be a subdirectory of the source directory
    CannotMoveBetweenSubdirectory,
    /// Prefix is not empty
    PrefixNotEmpty,
    IoError(io::Error),
    FdbError(error::FdbError),
    HcaError(HcaError),
    PackError(PackError),
    Other(String),
}

impl From<error::FdbError> for DirectoryError {
    fn from(err: error::FdbError) -> Self {
        DirectoryError::FdbError(err)
    }
}

impl From<HcaError> for DirectoryError {
    fn from(err: HcaError) -> Self {
        DirectoryError::HcaError(err)
    }
}

impl From<PackError> for DirectoryError {
    fn from(err: PackError) -> Self {
        DirectoryError::PackError(err)
    }
}
