//! Android/AArch64 cache maintenance around Cranelift's system memory provider.
//! Allocation, W^X protection, process-wide pipeline synchronization and freeing
//! remain with Cranelift; compiler-rt explicitly synchronizes each new code range.
use cranelift_jit::{BranchProtection, JITMemoryProvider, SystemMemoryProvider};
use cranelift_module::ModuleResult;
use std::{ffi::c_void, io};

unsafe extern "C" {
    // Provided by the NDK compiler-rt builtins archive linked by build.rs.
    fn __clear_cache(start: *mut c_void, end: *mut c_void);
}

pub(crate) struct AndroidMemory {
    inner: SystemMemoryProvider,
    pending_code: Vec<(*mut u8, usize)>,
}

// SAFETY: this wrapper exclusively owns its provider. Pending pointers describe
// that provider's stable allocations and are used only by its exclusive methods.
// Finalized modules can move to their execution owner without concurrent writes.
unsafe impl Send for AndroidMemory {}

impl Default for AndroidMemory {
    fn default() -> Self {
        Self {
            inner: SystemMemoryProvider::new(),
            pending_code: Vec::new(),
        }
    }
}

impl JITMemoryProvider for AndroidMemory {
    fn allocate_readexec(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        let pointer = self.inner.allocate_readexec(size, align)?;
        if size != 0 {
            self.pending_code.push((pointer, size));
        }
        Ok(pointer)
    }

    fn allocate_readwrite(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        self.inner.allocate_readwrite(size, align)
    }

    fn allocate_readonly(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        self.inner.allocate_readonly(size, align)
    }

    fn finalize(&mut self, branch_protection: BranchProtection) -> ModuleResult<()> {
        // JITModule has completed code writes and relocations before this call.
        // Flush while the new ranges are writable, before Cranelift changes them
        // to RX and broadcasts its membarrier synchronization to execution cores.
        for &(pointer, size) in &self.pending_code {
            // SAFETY: the provider owns this nonempty, initialized code range;
            // no entry into it is published until finalization succeeds.
            unsafe { __clear_cache(pointer.cast(), pointer.add(size).cast()) };
        }
        self.inner.finalize(branch_protection)?;
        // A later kernel/body/wrapper batch must never revisit published RX code.
        self.pending_code.clear();
        Ok(())
    }

    unsafe fn free_memory(&mut self) {
        // SAFETY: the caller guarantees that all entry pointers are retired and
        // no generated call is active, as required by the delegated provider.
        unsafe { self.inner.free_memory() };
        self.pending_code.clear();
    }
}
