//! Shared file positions: duplicate handles and processes refer to one cell.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

#[derive(Default)]
pub(crate) struct States(RefCell<Vec<Option<Entry>>>);

#[derive(Clone)]
pub(crate) struct Entry {
    pub fd: i32,
    pub kind: i32,
    pub position: Rc<Cell<i64>>,
    pub immutable: Option<[u8; 22]>,
}

thread_local! {
    static POSITIONS: RefCell<HashMap<usize, Weak<Cell<i64>>>> = RefCell::new(HashMap::new());
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
        identity: usize,
        immutable: Option<[u8; 22]>,
        kind: i32,
    ) {
        let position = POSITIONS.with_borrow_mut(|positions| {
            if let Some(position) = positions.get(&identity).and_then(Weak::upgrade) {
                return position;
            }
            // SAFETY: descriptor admission established a live regular file.
            let at = if kind == 1 {
                unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) }
            } else {
                0
            };
            let position = Rc::new(Cell::new(at));
            positions.insert(identity, Rc::downgrade(&position));
            position
        });
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
