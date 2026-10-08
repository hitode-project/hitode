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
struct Arena {
    base: usize,
    free: Vec<Range<usize>>,
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
            .checked_add(page - 1)
            .ok_or(io::ErrorKind::OutOfMemory)?
            / page
            * page;
        let mut arena = arena()?.lock().unwrap();
        let index = arena
            .free
            .iter()
            .position(|span| span.len() >= len)
            .ok_or(io::ErrorKind::OutOfMemory)?;
        let offset = arena.free[index].start;
        arena.free[index].start += len;
        if arena.free[index].is_empty() {
            arena.free.remove(index);
        }
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
        let mut arena = arena().unwrap().lock().unwrap();
        arena.free.push(self.offset..self.offset + self.len);
        arena.free.sort_unstable_by_key(|span| span.start);
        let mut index = 1;
        while index < arena.free.len() {
            if arena.free[index - 1].end == arena.free[index].start {
                arena.free[index - 1].end = arena.free[index].end;
                arena.free.remove(index);
            } else {
                index += 1;
            }
        }
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
