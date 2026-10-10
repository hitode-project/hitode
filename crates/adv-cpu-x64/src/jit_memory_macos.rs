//! Apple Silicon JIT storage. One process-wide MAP_JIT reservation also works
//! with Hardened Runtime's single-region rule (the app must carry allow-jit).
//! Hardware permissions are per-thread RW or RX, never simultaneously W+X.
use cranelift_jit::{BranchProtection, JITMemoryProvider, SystemMemoryProvider};
use cranelift_module::ModuleResult;
use std::cell::Cell;
use std::io;
use std::marker::PhantomData;
use std::ops::Range;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

unsafe extern "C" {
    fn pthread_jit_write_protect_np(enabled: libc::c_int);
    fn sys_icache_invalidate(start: *mut libc::c_void, len: usize);
}

thread_local! { static WRITING: Cell<usize> = const { Cell::new(0) }; }

/// Code generation/relocation never calls generated code. Restore execution
/// permission on every return, including compilation errors and unwinding.
/// The scope cannot move to another thread; other threads stay executable.
pub(crate) struct WriteScope(PhantomData<Rc<()>>);
impl WriteScope {
    pub(crate) fn new() -> Self {
        WRITING.with(|depth| {
            if depth.get() == 0 {
                unsafe { pthread_jit_write_protect_np(0) };
            }
            depth.set(depth.get() + 1);
        });
        Self(PhantomData)
    }
}
impl Drop for WriteScope {
    fn drop(&mut self) {
        WRITING.with(|depth| {
            depth.set(depth.get() - 1);
            if depth.get() == 0 {
                unsafe { pthread_jit_write_protect_np(1) };
            }
        });
    }
}

/// Executing cores synchronize their pipeline after acquiring a published
/// owner. Cache invalidation by the compiler alone is not an acquiring ISB.
#[inline]
pub(crate) fn synchronize_execution() {
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
}

const CAPACITY: usize = 1 << 30;
/// Allocation granule: Apple's 128-byte cache line. Regions share 16 KiB
/// pages (MAP_JIT W^X is per thread, so publishing one region never revokes
/// execution of a neighbor), but never a cache line, so one region's writes
/// and invalidation stay disjoint from code another core may be fetching.
const GRANULE: usize = 128;
struct Arena {
    base: usize,
    /// Sorted, disjoint, non-adjacent offsets of unallocated bytes.
    free: Vec<Range<usize>>,
}
impl Arena {
    /// First fit. `len` is a granule multiple and `align` a power of two no
    /// larger than a page; the page-aligned base makes offset alignment real.
    fn take(&mut self, len: usize, align: usize) -> Option<usize> {
        let align = align.max(GRANULE);
        let (index, start) = self.free.iter().enumerate().find_map(|(index, span)| {
            let start = span.start.checked_next_multiple_of(align)?;
            (start.checked_add(len)? <= span.end).then_some((index, start))
        })?;
        let span = self.free[index].clone();
        let end = start + len;
        match (span.start < start, end < span.end) {
            (false, false) => drop(self.free.remove(index)),
            (false, true) => self.free[index].start = end,
            (true, false) => self.free[index].end = start,
            (true, true) => {
                self.free[index].end = start;
                self.free.insert(index + 1, end..span.end);
            }
        }
        Some(start)
    }

    fn give(&mut self, mut span: Range<usize>) {
        let index = self.free.partition_point(|free| free.start < span.start);
        if self
            .free
            .get(index)
            .is_some_and(|next| next.start == span.end)
        {
            span.end = self.free.remove(index).end;
        }
        match index
            .checked_sub(1)
            .map(|previous| &mut self.free[previous])
        {
            Some(previous) if previous.end == span.start => previous.end = span.end,
            _ => self.free.insert(index, span),
        }
    }
}
fn arena() -> io::Result<&'static Mutex<Arena>> {
    static ARENA: OnceLock<io::Result<Mutex<Arena>>> = OnceLock::new();
    ARENA
        .get_or_init(|| {
            // MAP_JIT needs the RWX maximum mask; APRR enforces the current
            // thread's RW/RX selection, including while this mapping is created.
            let raw = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    CAPACITY,
                    libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                    libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                    -1,
                    0,
                )
            };
            if raw == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            Ok(Mutex::new(Arena {
                base: raw as usize,
                free: vec![0..CAPACITY],
            }))
        })
        .as_ref()
        .map_err(|e| io::Error::new(e.kind(), e.to_string()))
}

pub(crate) struct Code {
    pub(crate) pointer: NonNull<u8>,
    pub(crate) len: usize,
    offset: usize,
}
// An allocation has one compiler writer before publication; its owner pins
// immutable code until all execution leases are retired.
unsafe impl Send for Code {}
impl Code {
    fn allocate(size: usize, align: u64) -> io::Result<Self> {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let align = usize::try_from(align).map_err(|_| io::ErrorKind::InvalidInput)?;
        if !align.is_power_of_two() || align > page {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported JIT alignment",
            ));
        }
        let len = size
            .max(1)
            .checked_next_multiple_of(GRANULE)
            .ok_or(io::ErrorKind::OutOfMemory)?;
        let mut arena = arena()?.lock().unwrap();
        let offset = arena.take(len, align).ok_or(io::ErrorKind::OutOfMemory)?;
        Ok(Self {
            pointer: NonNull::new((arena.base + offset) as *mut u8).unwrap(),
            len,
            offset,
        })
    }

    pub(crate) fn new(words: &[u32]) -> Result<Self, String> {
        let _write = WriteScope::new();
        let code = Self::allocate(std::mem::size_of_val(words), 4).map_err(|e| e.to_string())?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                code.pointer.as_ptr(),
                std::mem::size_of_val(words),
            );
        }
        code.publish();
        Ok(code)
    }

    fn publish(&self) {
        // Apple's cache API includes the architecture's instruction visibility
        // maintenance. The compiler's channel supplies release/acquire ownership
        // transfer; the receiving thread never enters this thread's write scope.
        unsafe { sys_icache_invalidate(self.pointer.as_ptr().cast(), self.len) };
    }
}
impl Drop for Code {
    fn drop(&mut self) {
        arena()
            .unwrap()
            .lock()
            .unwrap()
            .give(self.offset..self.offset + self.len);
    }
}

pub(crate) struct MacMemory {
    data: SystemMemoryProvider,
    code: Vec<Code>,
    published: usize,
}
impl Default for MacMemory {
    fn default() -> Self {
        Self {
            data: SystemMemoryProvider::new(),
            code: Vec::new(),
            published: 0,
        }
    }
}
impl JITMemoryProvider for MacMemory {
    fn allocate_readexec(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        let code = Code::allocate(size, align)?;
        let pointer = code.pointer.as_ptr();
        self.code.push(code);
        Ok(pointer)
    }
    fn allocate_readonly(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        self.data.allocate_readonly(size, align)
    }
    fn allocate_readwrite(&mut self, size: usize, align: u64) -> io::Result<*mut u8> {
        self.data.allocate_readwrite(size, align)
    }
    fn finalize(&mut self, protection: BranchProtection) -> ModuleResult<()> {
        self.data.finalize(protection)?;
        for code in &self.code[self.published..] {
            code.publish();
        }
        self.published = self.code.len();
        Ok(())
    }
    unsafe fn free_memory(&mut self) {
        self.code.clear();
        self.published = 0;
        unsafe { self.data.free_memory() };
    }
}
impl Drop for MacMemory {
    fn drop(&mut self) {
        unsafe { self.free_memory() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(value: u32) -> Code {
        Code::new(&[0x52800000 | (value << 5), 0xd65f03c0]).unwrap()
    }
    unsafe fn run(code: &Code) -> u32 {
        synchronize_execution();
        let entry: unsafe extern "C" fn() -> u32 =
            unsafe { std::mem::transmute(code.pointer.as_ptr()) };
        unsafe { entry() }
    }

    #[test]
    fn arena_packs_aligned_regions_and_coalesces_released_spans() {
        let mut arena = Arena {
            base: 0,
            free: vec![0..CAPACITY],
        };
        let first = arena.take(GRANULE, 4).unwrap();
        let second = arena.take(GRANULE, 4).unwrap();
        assert_eq!((first, second), (0, GRANULE));
        // An over-aligned region leaves the skipped prefix for later regions.
        let aligned = arena.take(GRANULE, 1024).unwrap();
        assert_eq!(aligned, 1024);
        let third = arena.take(GRANULE, 4).unwrap();
        assert_eq!(third, 2 * GRANULE);
        for start in [first, aligned, second, third] {
            arena.give(start..start + GRANULE);
        }
        assert_eq!(arena.free, vec![0..CAPACITY]);
        assert_eq!(arena.take(CAPACITY + GRANULE, 4), None);
    }

    #[test]
    fn neighboring_regions_publish_without_revoking_execution() {
        let first = constant(1);
        assert_eq!(first.len, GRANULE);
        // Other tests share the process arena, so only the alignment of each
        // region, not adjacency, is deterministic here.
        let mut regions: Vec<_> = (2..=64).map(constant).collect();
        assert!(
            regions
                .iter()
                .all(|code| code.pointer.as_ptr() as usize % GRANULE == 0)
        );
        regions.retain(|code| unsafe { run(code) } % 2 == 0);
        regions.extend((65..=96).map(constant));
        assert_eq!(unsafe { run(&first) }, 1);
        for (code, value) in regions.iter().zip((2..=64).step_by(2).chain(65..=96)) {
            assert_eq!(unsafe { run(code) }, value);
        }
    }

    #[test]
    fn background_publication_keeps_the_execution_thread_executable() {
        let (published, entries) = std::sync::mpsc::sync_channel(0);
        let (finished, next) = std::sync::mpsc::sync_channel(0);
        let writer = std::thread::spawn(move || {
            for value in 1..=32 {
                let code = constant(value);
                published.send(code).unwrap();
                // This thread is writable while the receiver executes the
                // published entry. Its permissions must not affect that core.
                let _write = WriteScope::new();
                published.send(constant(value + 32)).unwrap();
                next.recv().unwrap();
            }
        });
        for value in 1..=32 {
            let first = entries.recv().unwrap();
            let second = entries.recv().unwrap();
            assert_eq!(unsafe { run(&first) }, value);
            assert_eq!(unsafe { run(&second) }, value + 32);
            finished.send(()).unwrap();
        }
        writer.join().unwrap();
        // An aborted compilation must restore executable permission too.
        let code = constant(7);
        let _ = std::panic::catch_unwind(|| {
            let _write = WriteScope::new();
            panic!("synthetic compilation error");
        });
        assert_eq!(unsafe { run(&code) }, 7);
    }

    #[test]
    fn jit_thread_permissions_never_allow_write_and_execute_together() {
        use std::os::unix::process::ExitStatusExt;
        if let Ok(mode) = std::env::var("ADV_MAC_JIT_PROBE") {
            let code = constant(1);
            if mode == "write" {
                unsafe { std::ptr::write_volatile(code.pointer.as_ptr(), 0) };
            } else {
                let _write = WriteScope::new();
                unsafe { run(&code) };
            }
            panic!("JIT protection did not reject the forbidden access");
        }
        for mode in ["write", "execute"] {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "jit_memory_macos::tests::jit_thread_permissions_never_allow_write_and_execute_together"])
                .env("ADV_MAC_JIT_PROBE", mode).output().unwrap();
            assert!(
                matches!(child.status.signal(), Some(libc::SIGBUS | libc::SIGSEGV)),
                "{mode}: {:?}",
                child.status
            );
        }
    }
}
