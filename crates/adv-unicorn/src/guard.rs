//! Unicorn instances that are safe to create and close on many threads at once.
//!
//! Unicorn 2.1 on Windows commits its translation buffer lazily: every instance adds a
//! process-wide vectored exception handler, and each access violation on any thread calls
//! the handlers of all live instances. `uc_close` removes its handler and frees the
//! handler's trampoline without waiting for threads that are dispatching through it, and
//! such a thread then dies with a stack overflow (about one parallel test run in thirty).
//!
//! The handler commits 4 MiB per fault, so a buffer of exactly that size is committed whole
//! by the first fault, which happens while Unicorn initializes its engine and generates the
//! prologue. Initialization therefore runs under a shared lock and closing under the
//! exclusive one; `emu_start` never faults and needs no lock, so a run is never delayed by
//! another thread's close. Translations that outgrow the buffer flush it instead.

use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::{PoisonError, RwLock};

use unicorn_engine::Unicorn;
use unicorn_engine::unicorn_const::{Arch, ControlType, Mode, Query, uc_ctl, uc_error};

static LIFETIMES: RwLock<()> = RwLock::new(());

/// The translation buffer size, equal to one commit of Unicorn's exception handler.
const TCG_BUFFER_SIZE: u32 = 4 << 20;

/// A [`Unicorn`] whose initialization and close follow the lock described above. It
/// dereferences to the engine.
#[repr(transparent)]
pub struct Guarded<'a, D: 'a>(ManuallyDrop<Unicorn<'a, D>>);

impl<'a> Guarded<'a, ()> {
    pub fn new(arch: Arch, mode: Mode) -> Result<Self, uc_error> {
        Self::new_with_data(arch, mode, ())
    }
}

impl<'a, D: 'a> Guarded<'a, D> {
    pub fn new_with_data(arch: Arch, mode: Mode, data: D) -> Result<Self, uc_error> {
        let uc = Self(ManuallyDrop::new(Unicorn::new_with_data(arch, mode, data)?));
        let control = ControlType(ControlType::TCG_BUFFER_SIZE.0 | ControlType::IO_WRITE.0);
        // SAFETY: the handle is live and this control takes one uint32 argument.
        let result: Result<(), uc_error> =
            unsafe { uc_ctl(uc.get_handle(), control, TCG_BUFFER_SIZE) }.into();
        result?;
        // Any query initializes the engine now rather than in a later, unguarded call. A
        // failed query drops the engine after the shared lock is released.
        let initialized = {
            let _shared = LIFETIMES.read().unwrap_or_else(PoisonError::into_inner);
            uc.query(Query::PAGE_SIZE)
        };
        initialized?;
        Ok(uc)
    }
}

impl<'a, D: 'a> Deref for Guarded<'a, D> {
    type Target = Unicorn<'a, D>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, D: 'a> DerefMut for Guarded<'a, D> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a, D: 'a> Drop for Guarded<'a, D> {
    fn drop(&mut self) {
        let _closing = LIFETIMES.write().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: the engine is dropped exactly once, here.
        unsafe { ManuallyDrop::drop(&mut self.0) };
    }
}
