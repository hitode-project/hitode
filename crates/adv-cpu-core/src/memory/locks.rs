//! Physical page locking is unsupported on the product hosts.
use crate::backend::PageLockError;

use super::Memory;

impl Memory {
    pub fn retire_locked_pages(&mut self) -> Result<(), PageLockError> {
        Ok(())
    }

    pub fn has_locked_pages(&self, _addr: u32, _len: u32) -> bool {
        false
    }

    pub fn lock_pages(&mut self, _addr: u32, _len: u32) -> Result<(), PageLockError> {
        Err(PageLockError::Unsupported)
    }

    pub fn unlock_pages(&mut self, _addr: u32, _len: u32) -> Result<(), PageLockError> {
        Err(PageLockError::Unsupported)
    }
}
