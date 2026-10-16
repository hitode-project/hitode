//! Page-owned publication storage, shareable with Metal without an upload copy.
use std::alloc::{Layout, handle_alloc_error};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

pub(crate) struct Pixels {
    pointer: NonNull<u8>,
    length: usize,
    allocation: usize,
}
// Mutation requires exclusive ownership. Published frames and Metal buffers
// retain an Arc; a writer replaces storage while either reader still owns it.
unsafe impl Send for Pixels {}
unsafe impl Sync for Pixels {}
impl Pixels {
    pub(crate) fn new() -> Self {
        Self {
            pointer: NonNull::dangling(),
            length: 0,
            allocation: 0,
        }
    }
    pub(crate) fn resize(&mut self, length: usize, value: u8) {
        if length == self.length {
            return;
        }
        if length == 0 {
            *self = Self::new();
            return;
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let allocation = length.next_multiple_of(page);
        // Metal requires a single VM region, not merely an aligned malloc.
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                allocation,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if pointer == libc::MAP_FAILED {
            handle_alloc_error(Layout::from_size_align(allocation, page).unwrap());
        }
        let pointer = NonNull::new(pointer.cast()).unwrap();
        let mut next = Self {
            pointer,
            length,
            allocation,
        };
        let keep = self.length.min(length);
        next[..keep].copy_from_slice(&self[..keep]);
        next[keep..].fill(value);
        *self = next;
    }
    pub(crate) fn allocation_size(&self) -> usize {
        self.allocation
    }
}
impl Clone for Pixels {
    fn clone(&self) -> Self {
        let mut copy = Self::new();
        copy.resize(self.length, 0);
        copy.copy_from_slice(self);
        copy
    }
}
impl Deref for Pixels {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.length) }
    }
}
impl DerefMut for Pixels {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.length) }
    }
}
impl Drop for Pixels {
    fn drop(&mut self) {
        if self.length != 0 {
            unsafe { libc::munmap(self.pointer.as_ptr().cast(), self.allocation) };
        }
    }
}
