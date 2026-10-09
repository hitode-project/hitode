//! Owner-side migration and protection for the optional direct window.
use super::*;

impl Memory {
    pub(crate) fn direct_memory_inline(&self) -> bool {
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        return self
            .direct_engine
            .as_ref()
            .is_some_and(|engine| engine.inline != 0);
        #[cfg(not(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        )))]
        false
    }

    pub(crate) fn direct_engine_pointer(&self) -> *const std::ffi::c_void {
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        return self
            .direct_engine
            .as_ref()
            .map_or(std::ptr::null(), |engine| {
                (engine as *const crate::direct_memory::Engine).cast()
            });
        #[cfg(not(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        )))]
        std::ptr::null()
    }

    #[cfg(all(
        test,
        any(
            target_os = "linux",
            target_os = "android",
            all(target_os = "macos", target_arch = "aarch64")
        ),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(crate) fn direct_test_pages(&mut self, size: usize) -> Result<(), String> {
        self.install_direct_window(
            crate::virtual_memory::Reservation::new_direct_with_host_page_size_for_test(
                self.len + 16384,
                size,
            )?,
            crate::direct_memory::Engine::new()?,
        )
    }
    pub(crate) fn direct_memory_enabled(&self) -> bool {
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        return self.direct_engine.is_some();
        #[cfg(not(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        )))]
        false
    }

    pub(crate) fn enable_direct_memory(&mut self) -> Result<(), String> {
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        {
            if self.direct_memory_enabled() {
                return Ok(());
            }
            if self.is_parallel() || !self.resident.is_empty() || !self.locked_pages.is_empty() {
                return Err("direct memory requires a serial owner without resident file views or page pins".into());
            }
            let engine = crate::direct_memory::Engine::new()?;
            // Keep a protected tail even for a complete 32-bit address space.
            let window = crate::virtual_memory::Reservation::new_direct(self.len + 16384)?;
            self.install_direct_window(window, engine)
        }
        #[cfg(not(all(
            any(
                target_os = "linux",
                target_os = "android",
                all(target_os = "macos", target_arch = "aarch64")
            ),
            any(target_arch = "x86_64", target_arch = "aarch64")
        )))]
        Err("direct memory requires Linux/Android x64 or ARM64, or macOS ARM64".into())
    }

    #[cfg(all(
        any(
            target_os = "linux",
            target_os = "android",
            all(target_os = "macos", target_arch = "aarch64")
        ),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fn install_direct_window(
        &mut self,
        window: crate::virtual_memory::Reservation,
        mut engine: crate::direct_memory::Engine,
    ) -> Result<(), String> {
        use crate::virtual_memory::GuestPageProtection;
        engine.window = window.ptr.as_ptr() as usize;
        engine.host_page_size = window.direct_host_page_size();
        let alias = window.checked_pointer().unwrap();
        let mut first = 0;
        while first < self.mapped.len() {
            if !self.mapped[first]
                || (self.perms[first] & SHARED != 0
                    && !self.private_copy_pages.contains_key(&first))
            {
                first += 1;
                continue;
            }
            let permissions = self.perms[first] & 7;
            let mut end = first + 1;
            while end < self.mapped.len()
                && self.mapped[end]
                && self.perms[end] & 7 == permissions
                && self.watched[end] == self.watched[first]
                && self.private_copy_pages.get(&end) == self.private_copy_pages.get(&first)
                && (self.perms[end] & SHARED == 0 || self.private_copy_pages.contains_key(&end))
            {
                end += 1;
            }
            let start = first * PAGE_SIZE;
            let len = (end - first) * PAGE_SIZE;
            window.commit_direct(
                start,
                len,
                GuestPageProtection::new(
                    permissions,
                    (permissions & EXEC != 0 && self.watched[first])
                        || self.private_copy_pages.get(&first) == Some(&true),
                )?,
            )?;
            // The old owner is software checked and quiescent. Its private
            // backing is readable even for guest NOACCESS/execute-only pages.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.ptr.as_ptr().add(start),
                    alias.as_ptr().add(start),
                    len,
                );
            }
            first = end;
        }
        // Publish only after every allocation/protection/copy has succeeded.
        if self.reservation.is_none() {
            unsafe {
                dealloc(
                    self.ptr.as_ptr(),
                    Layout::from_size_align(self.len, PAGE_SIZE).unwrap(),
                );
            }
        }
        self.ptr = alias;
        self.reservation = Some(window);
        self.direct_engine = Some(engine);
        self.fast.invalidate(0..self.mapped.len());
        self.epoch += 1;
        Ok(())
    }

    #[cfg(not(windows))]
    pub(super) fn protect_direct_pages(
        &self,
        pages: std::ops::Range<usize>,
        permissions: u8,
    ) -> Result<(), String> {
        if !self.direct_memory_enabled() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use crate::virtual_memory::GuestPageProtection;
            let window = self.reservation.as_ref().unwrap();
            let mut changed = Vec::new();
            for page in pages {
                // Shared overlays have no private direct mapping. Their checked
                // helpers retain the existing visibility and COW contracts.
                if self.perms[page] & SHARED != 0 && !self.private_copy_pages.contains_key(&page) {
                    continue;
                }
                let protection = GuestPageProtection::new(
                    permissions,
                    (permissions & EXEC != 0 && self.watched[page])
                        || self.private_copy_pages.get(&page) == Some(&true),
                )?;
                if let Err(error) = window.protect_direct(page * PAGE_SIZE, PAGE_SIZE, protection) {
                    for previous in changed {
                        let old = self.perms[previous];
                        if window
                            .protect_direct(
                                previous * PAGE_SIZE,
                                PAGE_SIZE,
                                GuestPageProtection::new(
                                    old & 7,
                                    (old & EXEC != 0 && self.watched[previous])
                                        || self.private_copy_pages.get(&previous) == Some(&true),
                                )?,
                            )
                            .is_err()
                        {
                            std::process::abort();
                        }
                    }
                    return Err(error);
                }
                changed.push(page);
            }
        }
        Ok(())
    }

    #[cfg(all(
        any(
            target_os = "linux",
            target_os = "android",
            all(target_os = "macos", target_arch = "aarch64")
        ),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(crate) fn direct_window(&self) -> &crate::virtual_memory::Reservation {
        self.reservation.as_ref().expect("direct memory owner")
    }

    #[cfg(not(windows))]
    pub(super) fn protect_direct_code(&self, pages: std::ops::Range<usize>) -> Result<(), String> {
        if !self.direct_memory_enabled() {
            return Ok(());
        }
        #[cfg(unix)]
        for page in pages {
            if self.mapped[page]
                && self.perms[page] & EXEC != 0
                && (self.perms[page] & SHARED == 0 || self.private_copy_pages.contains_key(&page))
            {
                self.reservation.as_ref().unwrap().protect_direct(
                    page * PAGE_SIZE,
                    PAGE_SIZE,
                    crate::virtual_memory::GuestPageProtection::new(self.perms[page] & 7, true)?,
                )?;
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    pub(super) fn restore_private_copy_direct(&self, pages: &[usize]) -> Result<(), String> {
        if !self.direct_memory_enabled() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use crate::virtual_memory::GuestPageProtection;
            let window = self.reservation.as_ref().unwrap();
            let mut restored = Vec::new();
            for &page in pages {
                let permissions = self.perms[page] & 7;
                let protection = GuestPageProtection::new(
                    permissions,
                    permissions & EXEC != 0 && self.watched[page],
                )?;
                if let Err(error) = window.protect_direct(page * PAGE_SIZE, PAGE_SIZE, protection) {
                    for previous in restored {
                        if window
                            .protect_direct(
                                previous * PAGE_SIZE,
                                PAGE_SIZE,
                                GuestPageProtection::new(self.perms[previous] & 7, true)?,
                            )
                            .is_err()
                        {
                            std::process::abort();
                        }
                    }
                    return Err(error);
                }
                restored.push(page);
            }
        }
        Ok(())
    }
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        all(target_os = "macos", target_arch = "aarch64")
    ),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod private_copy_tests {
    use super::*;
    use crate::virtual_memory::{FaultClassification, GuestAccess};
    #[test]
    fn private_copy_direct_migration_preserves_bytes_and_retires_store_barriers() {
        for enable_first in [false, true] {
            let mut memory = Memory::reserve(0x10000, 2 * PAGE_SIZE).unwrap();
            if enable_first {
                memory.enable_direct_memory().unwrap();
            }
            memory
                .map_private_copy_pages(0x10000, &[0x19; 2 * PAGE_SIZE], READ | WRITE)
                .unwrap();
            if !enable_first {
                memory.enable_direct_memory().unwrap();
            }
            let mut bytes = [0; 4];
            memory.read_into(0x10000, &mut bytes).unwrap();
            assert_eq!(bytes, [0x19; 4]);
            let address = memory.direct_window().ptr.as_ptr() as usize;
            assert_eq!(
                memory
                    .direct_window()
                    .direct_fault_view()
                    .unwrap()
                    .classify(address, GuestAccess::Write),
                FaultClassification::CodeWriteProtection
            );
            memory.write(0x10000, &[0x27]).unwrap();
            assert_eq!(
                memory
                    .direct_window()
                    .direct_fault_view()
                    .unwrap()
                    .classify(address, GuestAccess::Write),
                FaultClassification::Accessible
            );
            memory.protect(0x11000, PAGE_SIZE, 0).unwrap();
            memory.privatize_copy_pages(0x11000, PAGE_SIZE).unwrap();
            assert_eq!(
                memory
                    .direct_window()
                    .direct_fault_view()
                    .unwrap()
                    .classify(address + PAGE_SIZE, GuestAccess::Read),
                FaultClassification::AccessViolation
            );
            memory.read_into(0x11000, &mut bytes).unwrap();
            assert_eq!(bytes, [0x19; 4]);
            assert_eq!(memory.file_page_copy_on_write(0x11000), Some(false));
        }
    }
}
