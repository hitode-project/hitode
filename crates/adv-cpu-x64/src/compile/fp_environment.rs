//! Borrow the host FP environment once for a complete native linked chain.
//!
//! Guest x87 control words and MXCSR are canonical emulated state. They do not
//! configure ARM FPCR: admitted native binary64 operations require host nearest
//! rounding, gradual underflow and masked exceptions independently of the
//! caller's environment. Guest exception flags are computed by the lowering,
//! so native FPSR changes must not leak back to the host caller.
use std::marker::PhantomData;
use std::rc::Rc;

pub(crate) struct Guard {
    control: u64,
    status: u64,
    // A host register environment must be restored on the thread that saved it.
    _same_thread: PhantomData<Rc<()>>,
}

impl Guard {
    #[inline]
    pub(crate) fn enter() -> Self {
        let control: u64;
        let status: u64;
        // SAFETY: FPCR/FPSR are thread-local user-accessible ARM64 registers.
        // These reads touch no memory, stack or integer condition flags.
        unsafe {
            core::arch::asm!(
                "mrs {control}, fpcr",
                "mrs {status}, fpsr",
                control = out(reg) control,
                status = out(reg) status,
                options(nomem, nostack, preserves_flags),
            );
        }
        if control != 0 {
            set_control(0);
        }
        Self {
            control,
            status,
            _same_thread: PhantomData,
        }
    }
}

impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        let control: u64;
        // SAFETY: this thread-bound guard restores the exact register image
        // saved on entry, including on Rust early returns and unwinding.
        unsafe {
            core::arch::asm!(
                "mrs {control}, fpcr",
                control = out(reg) control,
                options(nomem, nostack, preserves_flags),
            );
        }
        if control != self.control {
            set_control(self.control);
        }
        // Preserve the caller's sticky exception and saturation flags exactly,
        // even when its control word was already the default on entry.
        // SAFETY: status was read from FPSR on this same thread by enter().
        unsafe {
            core::arch::asm!(
                "msr fpsr, {status}",
                status = in(reg) self.status,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
}

#[inline]
fn set_control(control: u64) {
    // SAFETY: only zero or a value read from this thread's FPCR is written.
    // The operation changes FP control, not memory or integer condition flags.
    unsafe {
        core::arch::asm!(
            "msr fpcr, {control}",
            control = in(reg) control,
            options(nomem, nostack, preserves_flags),
        );
    }
}
