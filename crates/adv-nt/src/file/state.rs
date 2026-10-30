//! Shared file positions: the server fd retains the cell across duplicate and
//! inherited handles, including clients that have not admitted a descriptor.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default)]
pub(crate) struct States(RefCell<Vec<Option<Entry>>>);

#[derive(Clone)]
pub(crate) struct Entry {
    pub fd: i32,
    pub kind: i32,
    pub position: Rc<Cell<i64>>,
    pub immutable: Option<[u8; 22]>,
}

impl States {
    pub fn get(&self, handle: u32) -> Option<Entry> {
        if handle & 3 != 0 {
            return None;
        }
        self.0.borrow().get((handle >> 2) as usize)?.clone()
    }

    pub fn remove(&self, handle: u32) {
        if let Some(entry) = self.0.borrow_mut().get_mut((handle >> 2) as usize) {
            *entry = None;
        }
    }

    pub fn insert(
        &self,
        handle: u32,
        fd: i32,
        position: Rc<Cell<i64>>,
        immutable: Option<[u8; 22]>,
        kind: i32,
    ) {
        let mut entries = self.0.borrow_mut();
        let index = (handle >> 2) as usize;
        let len = entries.len().max(index + 1);
        entries.resize_with(len, || None);
        entries[index] = Some(Entry {
            fd,
            kind,
            position,
            immutable,
        });
    }
}

impl Entry {
    pub fn size(&self) -> Result<u64, u32> {
        if let Some(bytes) = self.immutable {
            return Ok(u64::from_le_bytes(bytes[8..16].try_into().unwrap()));
        }
        if self.kind == 3 {
            return Err(crate::status::NOT_IMPLEMENTED);
        }
        if self.kind != 1 {
            return Ok(0);
        }
        // SAFETY: the cached handle retains this regular-file descriptor.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(self.fd, &mut stat) } != 0 {
            return Err(super::write_error());
        }
        Ok(stat.st_size as u64)
    }
}
