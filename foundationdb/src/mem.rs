/// Utilities to handle FoundationDB's allocation
/// Foundationdb uses arenas for allocation which are not aligned.
/// https://github.com/apple/foundationdb/blob/7aa578f616c24b60436429645427485b97520286/flow/Arena.cpp#L28-L31
///
/// Rust does not allow dereferencing unaligned pointers, so we copy the memory first to an aligned
/// pointer before constructing our slice.

use std::{alloc::Layout, ptr::copy_nonoverlapping};

pub(crate) unsafe fn read_unaligned_slice<T>(src: *const T, len: usize) -> *const [T] {
    let layout = Layout::array::<T>(len).expect("failed to create slice memory layout");
    let aligned = std::alloc::alloc(layout);
    copy_nonoverlapping(src as *const u8, aligned, layout.size());
    std::slice::from_raw_parts(aligned as *const T, len)
}

pub(crate) unsafe fn read_unaligned_struct<T>(src: *const T) -> *const T {
    let layout = Layout::new::<T>();
    let aligned = std::alloc::alloc(layout);
    copy_nonoverlapping(src as *const u8, aligned, layout.size());
    aligned as *const T
}
