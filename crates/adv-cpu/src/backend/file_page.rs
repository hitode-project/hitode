//! Shared data pages within the single guest execution worker.
//!
//! These pages intentionally cannot cross host threads. Device DMA storage has
//! its separate atomic/readonly-alias contract; file pages must never be sent to
//! an audio callback or exposed as borrowed Rust slices.
use std::cell::{Cell, RefCell, UnsafeCell};
use std::rc::Rc;

pub const FILE_PAGE_SIZE: usize = 4096;

#[repr(align(4096))]
struct Bytes([u8; FILE_PAGE_SIZE]);

pub struct FilePage {
    state: RefCell<PageState>,
    dirty: Cell<bool>,
    // Keep even an immutable FilePage reference confined to the owning worker.
    _worker: std::marker::PhantomData<Rc<()>>,
}

type Initializer = Box<dyn Fn() -> Result<[u8; FILE_PAGE_SIZE], String>>;
enum PageState {
    Lazy(Initializer),
    Loading,
    Ready(Box<UnsafeCell<Bytes>>),
    /// The bytes live in one guest window page, where native code accesses
    /// them as ordinary memory, until `evict` moves them back. Native stores
    /// are untracked, so `clean` keeps the bytes as of the last clean state.
    Resident {
        window: *mut u8,
        clean: Box<UnsafeCell<Bytes>>,
    },
}

/// Whether a resident window differs from its clean copy.
///
/// # Safety
/// `window` must be a live resident window of the same worker.
unsafe fn changed(window: *const u8, clean: &UnsafeCell<Bytes>) -> bool {
    // SAFETY: both are page sized; no store can run during the comparison.
    unsafe {
        std::slice::from_raw_parts(window, FILE_PAGE_SIZE)
            != std::slice::from_raw_parts((*clean.get()).0.as_ptr(), FILE_PAGE_SIZE)
    }
}

impl FilePage {
    pub fn new(bytes: [u8; FILE_PAGE_SIZE]) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(PageState::Ready(Box::new(UnsafeCell::new(Bytes(bytes))))),
            dirty: Cell::new(false),
            _worker: std::marker::PhantomData,
        })
    }

    pub fn lazy(
        initializer: impl Fn() -> Result<[u8; FILE_PAGE_SIZE], String> + 'static,
    ) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(PageState::Lazy(Box::new(initializer))),
            dirty: Cell::new(false),
            _worker: std::marker::PhantomData,
        })
    }

    pub fn initialized(&self) -> bool {
        matches!(
            *self.state.borrow(),
            PageState::Ready(_) | PageState::Resident { .. }
        )
    }

    pub(crate) fn resident(&self) -> bool {
        matches!(*self.state.borrow(), PageState::Resident { .. })
    }

    /// Moves the page's bytes into `window`, which stores them until `evict`.
    ///
    /// # Safety
    /// `window` must stay valid and writable for `FILE_PAGE_SIZE` bytes until
    /// `evict`, and be accessed only on this page's execution worker.
    pub(crate) unsafe fn make_resident(&self, window: *mut u8) -> Result<(), String> {
        let source = self.pointer()?;
        let PageState::Ready(clean) = self.state.replace(PageState::Loading) else {
            unreachable!("initialized pages are ready or resident")
        };
        // SAFETY: both ranges are page sized; the caller owns the window.
        unsafe { std::ptr::copy_nonoverlapping(source, window, FILE_PAGE_SIZE) };
        self.state.replace(PageState::Resident { window, clean });
        Ok(())
    }

    /// Moves a resident page's bytes back out of its window.
    pub(crate) fn evict(&self) {
        if !self.resident() {
            return;
        }
        let PageState::Resident { window, mut clean } = self.state.replace(PageState::Loading)
        else {
            unreachable!()
        };
        // SAFETY: `make_resident` keeps the window valid until this call.
        unsafe {
            if changed(window, &clean) {
                self.dirty.set(true);
            }
            std::ptr::copy_nonoverlapping(window, clean.get_mut().0.as_mut_ptr(), FILE_PAGE_SIZE)
        };
        self.state.replace(PageState::Ready(clean));
    }

    /// Resolve first-touch I/O before a transactional write changes any bytes.
    pub fn prepare(&self) -> Result<(), String> {
        self.pointer().map(|_| ())
    }

    pub fn read_into(&self, offset: usize, out: &mut [u8]) -> Result<(), String> {
        assert!(offset <= FILE_PAGE_SIZE && out.len() <= FILE_PAGE_SIZE - offset);
        // SAFETY: this Rc-confined page has no concurrent access and produces no
        // Rust references to its bytes. The caller owns the output buffer.
        unsafe { std::ptr::copy(self.pointer()?.add(offset), out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    pub fn write(&self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        assert!(offset <= FILE_PAGE_SIZE && bytes.len() <= FILE_PAGE_SIZE - offset);
        // SAFETY: same exclusive worker and UnsafeCell ownership as read_into.
        unsafe { std::ptr::copy(bytes.as_ptr(), self.pointer()?.add(offset), bytes.len()) };
        self.mark_dirty();
        Ok(())
    }

    pub fn snapshot(&self) -> Result<[u8; FILE_PAGE_SIZE], String> {
        let mut bytes = [0; FILE_PAGE_SIZE];
        self.read_into(0, &mut bytes)?;
        Ok(bytes)
    }

    pub fn dirty(&self) -> bool {
        self.dirty.get()
            || match &*self.state.borrow() {
                // SAFETY: a resident window stays live until `evict`.
                PageState::Resident { window, clean } => unsafe { changed(*window, clean) },
                _ => false,
            }
    }
    pub fn mark_dirty(&self) {
        self.dirty.set(true);
    }
    pub fn clear_dirty(&self) {
        self.dirty.set(false);
        if let PageState::Resident { window, clean } = &*self.state.borrow() {
            // SAFETY: a resident window stays live until `evict`, and the
            // clean copy has no outstanding references.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    *window,
                    (*clean.get()).0.as_mut_ptr(),
                    FILE_PAGE_SIZE,
                )
            };
        }
    }

    fn pointer(&self) -> Result<*mut u8, String> {
        match &*self.state.borrow() {
            PageState::Ready(bytes) => {
                // SAFETY: form a pointer to the UnsafeCell contents without a
                // reference to bytes that the worker/foreign engine can write.
                return Ok(unsafe { std::ptr::addr_of_mut!((*bytes.get()).0).cast() });
            }
            PageState::Resident { window, .. } => return Ok(*window),
            PageState::Loading => return Err("recursive file page initialization".into()),
            PageState::Lazy(_) => {}
        }
        let PageState::Lazy(initializer) = self.state.replace(PageState::Loading) else {
            unreachable!()
        };
        // No RefCell borrow is held across I/O or callbacks. A transient failure
        // keeps the initializer so a restarted instruction can retry the read.
        match initializer() {
            Ok(bytes) => {
                self.state
                    .replace(PageState::Ready(Box::new(UnsafeCell::new(Bytes(bytes)))));
                self.pointer()
            }
            Err(error) => {
                self.state.replace(PageState::Lazy(initializer));
                Err(error)
            }
        }
    }

    /// Stable foreign-engine memory for development-only CPU backends.
    ///
    /// # Safety
    /// Retain this page until the engine has unmapped the pointer. Access only
    /// on the same execution worker, without any Rust references to its bytes.
    pub unsafe fn foreign_pointer(&self) -> Result<*mut u8, String> {
        self.pointer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_pages_allocate_and_initialize_only_at_first_touch() {
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let page = FilePage::lazy(move || {
            count.set(count.get() + 1);
            Ok([0x5a; 4096])
        });
        assert!(!page.initialized());
        assert_eq!(calls.get(), 0);
        let alias = page.clone();
        let mut value = [0; 2];
        alias.read_into(4094, &mut value).unwrap();
        assert_eq!(value, [0x5a; 2]);
        assert_eq!(calls.get(), 1);
        assert!(page.initialized());
        page.write(0, &[1, 2, 3]).unwrap();
        assert_eq!(&alias.snapshot().unwrap()[..3], &[1, 2, 3]);
        assert_eq!(calls.get(), 1);
        assert!(page.dirty());
    }

    #[test]
    fn lazy_io_failure_can_be_retried_without_substituting_zeroes() {
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let page = FilePage::lazy(move || {
            count.set(count.get() + 1);
            if count.get() < 3 {
                Err("injected read failure".into())
            } else {
                Ok([0x37; 4096])
            }
        });
        let mut output = [0xa5; 4];
        assert!(page.read_into(0, &mut output).is_err());
        assert_eq!(output, [0xa5; 4]);
        assert!(page.write(0, &[0; 4]).is_err());
        assert!(!page.dirty());
        assert!(!page.initialized());
        assert_eq!(calls.get(), 2);
        page.read_into(0, &mut output).unwrap();
        assert_eq!(output, [0x37; 4]);
        assert_eq!(calls.get(), 3);
    }
}
