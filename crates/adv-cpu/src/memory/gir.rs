//! Serial mapping leases for SSA code. No helper or worker may enter a lease.
use super::{EXEC, Memory, PAGE_SIZE, READ, WRITE};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct LeaseCache {
    epoch: Option<u64>,
    identity: (usize, u32, usize),
    permissions: Arc<Vec<u8>>,
    owner_permissions: Option<Arc<Vec<u8>>>,
}

/// The exclusive borrow pins both the ordinary allocation and the direct
/// window's permanently accessible checked alias. Mapping/protection changes
/// require this borrow to end; asynchronous requesters only request an exit.
pub(crate) struct Lease<'a> {
    memory: &'a mut Memory,
    permissions: Arc<Vec<u8>>,
    pub epoch: u64,
}

impl LeaseCache {
    pub fn acquire<'a>(&mut self, memory: &'a mut Memory) -> Result<Lease<'a>, String> {
        if memory.is_parallel() || memory.foreign_access_required() {
            return Err("GIR requires an exclusive serial mapping lease".into());
        }
        let identity = (memory.ptr.as_ptr() as usize, memory.base, memory.len);
        if self.epoch != Some(memory.epoch)
            || self.identity != identity
            || !self
                .owner_permissions
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(p, &memory.perms))
        {
            self.permissions = Arc::new(
                memory
                    .perms
                    .iter()
                    .enumerate()
                    .map(|(page, &permission)| {
                        // Include resident file views: their external backing and
                        // aliases are outside the private scalar lease contract.
                        if !memory.mapped[page]
                            || permission & !(READ | WRITE | EXEC) != 0
                            || permission & EXEC != 0
                            || memory.shared_pages.contains_key(&page)
                            || memory.resident.contains_key(&page)
                        {
                            0
                        } else {
                            permission
                        }
                    })
                    .collect(),
            );
            self.epoch = Some(memory.epoch);
            self.identity = identity;
            // Pin a metadata identity too: allocator address reuse across two
            // owners must not turn a coincident epoch into a valid old grant.
            self.owner_permissions = Some(memory.perms.clone());
        }
        Ok(Lease {
            epoch: memory.epoch,
            memory,
            permissions: self.permissions.clone(),
        })
    }
}

impl Lease<'_> {
    pub fn window(&self) -> (u32, usize) {
        (self.memory.base, self.memory.len)
    }

    pub fn allows(&self, address: u32, width: usize, write: bool) -> bool {
        if width == 0 || width > 4 || u64::from(address) + width as u64 > 1 << 32 {
            return false;
        }
        let Ok(range) = self.memory.range(address, width) else {
            return false;
        };
        let required = if write { WRITE } else { READ };
        (range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE)
            .all(|page| self.permissions[page] & required != 0)
    }

    /// Only generated full-range checks or entry proofs may dereference this
    /// pointer. In direct mode it is the checked alias, never the protected view.
    pub fn data(&mut self) -> *mut u8 {
        self.memory.ptr.as_ptr()
    }
    pub fn permissions(&self) -> *const u8 {
        self.permissions.as_ptr()
    }
}
