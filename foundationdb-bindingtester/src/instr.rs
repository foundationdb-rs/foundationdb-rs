//! Instruction parsing: the instruction codes and their flags.

use foundationdb as fdb;
use foundationdb_sys as fdb_sys;

use fdb::tuple::{Bytes, Element, unpack};

use crate::fdb::options::{MutationType, StreamingMode};

pub(crate) fn mutation_from_str(s: &str) -> MutationType {
    match s {
        "ADD" => MutationType::Add,
        "AND" => MutationType::And,
        "BIT_AND" => MutationType::BitAnd,
        "OR" => MutationType::Or,
        "BIT_OR" => MutationType::BitOr,
        "XOR" => MutationType::Xor,
        "BIT_XOR" => MutationType::BitXor,
        "APPEND_IF_FITS" => MutationType::AppendIfFits,
        "MAX" => MutationType::Max,
        "MIN" => MutationType::Min,
        "SET_VERSIONSTAMPED_KEY" => MutationType::SetVersionstampedKey,
        "SET_VERSIONSTAMPED_VALUE" => MutationType::SetVersionstampedValue,
        "BYTE_MIN" => MutationType::ByteMin,
        "BYTE_MAX" => MutationType::ByteMax,
        "COMPARE_AND_CLEAR" => MutationType::CompareAndClear,
        _ => unimplemented!("mutation_from_str({})", s),
    }
}

pub(crate) fn streaming_from_value(val: i32) -> StreamingMode {
    match val {
        fdb_sys::FDBStreamingMode_FDB_STREAMING_MODE_WANT_ALL => StreamingMode::WantAll,
        fdb_sys::FDBStreamingMode_FDB_STREAMING_MODE_ITERATOR => StreamingMode::Iterator,
        fdb_sys::FDBStreamingMode_FDB_STREAMING_MODE_EXACT => StreamingMode::Exact,
        fdb_sys::FDBStreamingMode_FDB_STREAMING_MODE_SMALL => StreamingMode::Small,
        fdb_sys::FDBStreamingMode_FDB_STREAMING_MODE_MEDIUM => StreamingMode::Medium,
        fdb_sys::FDBStreamingMode_FDB_STREAMING_MODE_LARGE => StreamingMode::Large,
        fdb_sys::FDBStreamingMode_FDB_STREAMING_MODE_SERIAL => StreamingMode::Serial,
        _ => unimplemented!("streaming_from_value({})", val),
    }
}

pub(crate) struct Instr {
    pub(crate) code: InstrCode,
    pub(crate) database: bool,
    pub(crate) snapshot: bool,
    pub(crate) starts_with: bool,
    pub(crate) selector: bool,
}

impl std::fmt::Debug for Instr {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "[{:?}", self.code)?;
        if self.database {
            write!(fmt, " db")?;
        }
        if self.snapshot {
            write!(fmt, " snapshot")?;
        }
        if self.starts_with {
            write!(fmt, " starts_with")?;
        }
        if self.selector {
            write!(fmt, " selector")?;
        }
        write!(fmt, "]")
    }
}

impl Instr {
    pub(crate) fn pop_database(&mut self) -> bool {
        if self.database {
            self.database = false;
            true
        } else {
            false
        }
    }
    pub(crate) fn pop_snapshot(&mut self) -> bool {
        if self.snapshot {
            self.snapshot = false;
            true
        } else {
            false
        }
    }
    pub(crate) fn pop_starts_with(&mut self) -> bool {
        if self.starts_with {
            self.starts_with = false;
            true
        } else {
            false
        }
    }
    pub(crate) fn pop_selector(&mut self) -> bool {
        if self.selector {
            self.selector = false;
            true
        } else {
            false
        }
    }

    pub(crate) fn has_flags(&self) -> bool {
        self.database || self.snapshot || self.starts_with || self.selector
    }
}

#[derive(Debug)]
pub(crate) enum InstrCode {
    // data operations
    Push(Element<'static>),
    Dup,
    EmptyStack,
    Swap,
    Pop,
    Sub,
    Concat,
    LogStack,

    // foundationdb operations
    NewTransaction,
    UseTransaction,
    OnError,
    Get,
    GetKey,
    GetRange,
    GetReadVersion,
    GetVersionstamp,
    Set,
    SetReadVersion,
    Clear,
    ClearRange,
    AtomicOp,
    ReadConflictRange,
    WriteConflictRange,
    ReadConflictKey,
    WriteConflictKey,
    DisableWriteConflict,
    Commit,
    Reset,
    Cancel,
    GetCommittedVersion,
    GetApproximateSize,
    WaitFuture,
    GetEstimatedRangeSize,
    GetRangeSplitPoints,

    TuplePack,
    TuplePackWithVersionstamp,
    TupleUnpack,
    TupleRange,
    TupleSort,
    EncodeFloat,
    EncodeDouble,
    DecodeFloat,
    DecodeDouble,

    // Thread Operations
    StartThread,
    WaitEmpty,

    // misc
    UnitTests,

    // Directory/Subspace/Layer Creation
    DirectoryCreateSubspace,
    DirectoryCreateLayer,
    DirectoryCreateOrOpen,
    DirectoryCreate,
    DirectoryOpen,

    // Directory Management
    DirectoryChange,
    DirectorySetErrorIndex,

    // Directory Operations
    DirectoryMove,
    DirectoryMoveTo,
    DirectoryRemove,
    DirectoryRemoveIfExists,
    DirectoryList,
    DirectoryExists,

    // Subspace operation
    DirectoryPackKey,
    DirectoryUnpackKey,
    DirectoryRange,
    DirectoryContains,
    DirectoryOpenSubspace,

    // Directory Logging
    DirectoryLogSubspace,
    DirectoryLogDirectory,

    // Other
    DirectoryStripPrefix,
}

pub(crate) fn has_opt<'a>(cmd: &'a str, opt: &'static str) -> (&'a str, bool) {
    if cmd.ends_with(opt) {
        (&cmd[0..(cmd.len() - opt.len())], true)
    } else {
        (cmd, false)
    }
}

impl Instr {
    pub(crate) fn from(data: &[u8]) -> Self {
        use crate::InstrCode::*;

        let data = Bytes::from(data);
        trace!("inst {data:?}");
        let tup: Vec<Element> = unpack(&data).unwrap();
        let cmd = tup[0].as_str().unwrap();

        let (cmd, database) = has_opt(cmd, "_DATABASE");
        let (cmd, snapshot) = has_opt(cmd, "_SNAPSHOT");
        let (cmd, starts_with) = has_opt(cmd, "_STARTS_WITH");
        let (cmd, selector) = has_opt(cmd, "_SELECTOR");

        let code = match cmd {
            "PUSH" => Push(tup[1].clone().into_owned()),
            "DUP" => Dup,
            "EMPTY_STACK" => EmptyStack,
            "SWAP" => Swap,
            "POP" => Pop,
            "SUB" => Sub,
            "CONCAT" => Concat,
            "LOG_STACK" => LogStack,

            "NEW_TRANSACTION" => NewTransaction,
            "USE_TRANSACTION" => UseTransaction,
            "ON_ERROR" => OnError,
            "GET" => Get,
            "GET_KEY" => GetKey,
            "GET_RANGE" => GetRange,
            "GET_READ_VERSION" => GetReadVersion,
            "GET_VERSIONSTAMP" => GetVersionstamp,

            "SET" => Set,
            "SET_READ_VERSION" => SetReadVersion,
            "CLEAR" => Clear,
            "CLEAR_RANGE" => ClearRange,
            "ATOMIC_OP" => AtomicOp,
            "READ_CONFLICT_RANGE" => ReadConflictRange,
            "WRITE_CONFLICT_RANGE" => WriteConflictRange,
            "READ_CONFLICT_KEY" => ReadConflictKey,
            "WRITE_CONFLICT_KEY" => WriteConflictKey,
            "DISABLE_WRITE_CONFLICT" => DisableWriteConflict,
            "COMMIT" => Commit,
            "RESET" => Reset,
            "CANCEL" => Cancel,
            "GET_COMMITTED_VERSION" => GetCommittedVersion,
            "GET_APPROXIMATE_SIZE" => GetApproximateSize,
            "WAIT_FUTURE" => WaitFuture,
            "GET_ESTIMATED_RANGE_SIZE" => GetEstimatedRangeSize,
            "GET_RANGE_SPLIT_POINTS" => GetRangeSplitPoints,

            "TUPLE_PACK" => TuplePack,
            "TUPLE_PACK_WITH_VERSIONSTAMP" => TuplePackWithVersionstamp,
            "TUPLE_UNPACK" => TupleUnpack,
            "TUPLE_RANGE" => TupleRange,
            "TUPLE_SORT" => TupleSort,
            "ENCODE_FLOAT" => EncodeFloat,
            "ENCODE_DOUBLE" => EncodeDouble,
            "DECODE_FLOAT" => DecodeFloat,
            "DECODE_DOUBLE" => DecodeDouble,

            "START_THREAD" => StartThread,
            "WAIT_EMPTY" => WaitEmpty,

            "UNIT_TESTS" => UnitTests,

            "DIRECTORY_CREATE_SUBSPACE" => DirectoryCreateSubspace,
            "DIRECTORY_CREATE_LAYER" => DirectoryCreateLayer,
            "DIRECTORY_CREATE_OR_OPEN" => DirectoryCreateOrOpen,
            "DIRECTORY_CREATE" => DirectoryCreate,
            "DIRECTORY_OPEN" => DirectoryOpen,

            "DIRECTORY_CHANGE" => DirectoryChange,
            "DIRECTORY_SET_ERROR_INDEX" => DirectorySetErrorIndex,

            "DIRECTORY_MOVE" => DirectoryMove,
            "DIRECTORY_MOVE_TO" => DirectoryMoveTo,
            "DIRECTORY_REMOVE" => DirectoryRemove,
            "DIRECTORY_REMOVE_IF_EXISTS" => DirectoryRemoveIfExists,
            "DIRECTORY_LIST" => DirectoryList,
            "DIRECTORY_EXISTS" => DirectoryExists,

            "DIRECTORY_PACK_KEY" => DirectoryPackKey,
            "DIRECTORY_UNPACK_KEY" => DirectoryUnpackKey,
            "DIRECTORY_RANGE" => DirectoryRange,
            "DIRECTORY_CONTAINS" => DirectoryContains,
            "DIRECTORY_OPEN_SUBSPACE" => DirectoryOpenSubspace,

            "DIRECTORY_LOG_SUBSPACE" => DirectoryLogSubspace,
            "DIRECTORY_LOG_DIRECTORY" => DirectoryLogDirectory,

            "DIRECTORY_STRIP_PREFIX" => DirectoryStripPrefix,

            name => unimplemented!("inimplemented instr: {}", name),
        };
        Instr {
            code,
            database,
            snapshot,
            starts_with,
            selector,
        }
    }
}

impl InstrCode {
    /// Whether [`crate::StackMachine::run_directory_instr`] runs this instruction.
    pub(crate) fn is_directory(&self) -> bool {
        use InstrCode::*;
        matches!(
            self,
            DirectoryCreateSubspace
                | DirectoryCreateLayer
                | DirectoryCreate
                | DirectoryOpen
                | DirectoryCreateOrOpen
                | DirectoryChange
                | DirectorySetErrorIndex
                | DirectoryMove
                | DirectoryMoveTo
                | DirectoryRemove
                | DirectoryRemoveIfExists
                | DirectoryList
                | DirectoryExists
                | DirectoryPackKey
                | DirectoryUnpackKey
                | DirectoryRange
                | DirectoryContains
                | DirectoryOpenSubspace
                | DirectoryLogSubspace
                | DirectoryLogDirectory
                | DirectoryStripPrefix
        )
    }
}
