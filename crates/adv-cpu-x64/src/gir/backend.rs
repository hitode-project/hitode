//! Research-only exact-PC registration. Hardware tests run in child processes
//! so the existing platform bridge's singleton handler never replaces an active
//! production recovery session. The handler only copies a bounded frame image.
use super::{Frame, HARDWARE};
use std::cell::Cell;
use std::ffi::c_void;

#[derive(Clone, Debug)]
pub(super) struct Site {
    pub offset: u32,
    pub slots: [u32; 12],
}

struct Session<'a> {
    base: usize,
    end: usize,
    recovery: usize,
    sites: &'a [Site],
    window: usize,
    len: usize,
    captured: Option<Frame>,
}
thread_local! { static ACTIVE: Cell<*mut Session<'static>> = const { Cell::new(std::ptr::null_mut()) }; }

#[cfg(windows)]
unsafe extern "C" {
    fn adv_host_fault_install(
        dispatch: unsafe extern "C" fn(u32, usize, usize, usize, *mut c_void) -> usize,
    ) -> *mut c_void;
    fn adv_host_fault_gpr(context: *const c_void, register: u32) -> u64;
}
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe extern "C" {
    fn adv_posix_fault_install(
        dispatch: unsafe extern "C" fn(i32, usize, usize, usize, *mut c_void) -> usize,
    ) -> i32;
    fn adv_posix_fault_gpr(context: *const c_void, register: u32) -> u64;
}

#[cfg(windows)]
unsafe extern "C" fn dispatch(
    _: u32,
    pc: usize,
    address: usize,
    _: usize,
    context: *mut c_void,
) -> usize {
    let sp = unsafe { adv_host_fault_gpr(context, 4) } as usize;
    capture(pc, address, sp)
}
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe extern "C" fn dispatch(
    _: i32,
    pc: usize,
    address: usize,
    _: usize,
    context: *mut c_void,
) -> usize {
    let sp = unsafe { adv_posix_fault_gpr(context, 4) } as usize;
    capture(pc, address, sp)
}

fn capture(pc: usize, address: usize, sp: usize) -> usize {
    ACTIVE.with(|active| {
        let pointer = active.get();
        if pointer.is_null() {
            return 0;
        }
        // SAFETY: run freezes the borrowed code, metadata and native frame;
        // the callback runs synchronously on that same thread before return.
        let session = unsafe { &mut *pointer };
        if pc < session.base
            || pc >= session.end
            || address < session.window
            || address - session.window >= session.len
            || session.captured.is_some()
        {
            return 0;
        }
        let Some(site) = session
            .sites
            .iter()
            .find(|s| s.offset as usize == pc - session.base)
        else {
            return 0;
        };
        let words: [u32; 12] = site
            .slots
            .map(|offset| unsafe { ((sp + offset as usize) as *const u32).read_unaligned() });
        session.captured = Some(Frame {
            gpr: words[..8].try_into().unwrap(),
            flags: words[8],
            pc: words[9],
            committed: words[10],
            address: words[11],
        });
        session.recovery
    })
}

pub(super) unsafe fn run(
    code: &super::lower::Code,
    window: &super::memory::Window,
    call: impl FnOnce() -> u32,
) -> (u32, Option<Frame>) {
    assert!(
        std::env::var_os("ADV_GIR_FAULT_CHILD").is_some(),
        "hardware runner requires isolated child"
    );
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        #[cfg(windows)]
        assert!(!unsafe { adv_host_fault_install(dispatch) }.is_null());
        #[cfg(any(target_os = "linux", target_os = "android"))]
        assert_ne!(unsafe { adv_posix_fault_install(dispatch) }, 0);
    });
    let base = code.entry as usize;
    let mut session = Session {
        base,
        end: base + code.bytes,
        recovery: base + code.recovery as usize,
        sites: &code.sites,
        window: window.pointer() as usize,
        len: window.len + 16384,
        captured: None,
    };
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ACTIVE.with(|slot| slot.set(std::ptr::null_mut()));
        }
    }
    ACTIVE.with(|slot| {
        assert!(slot.get().is_null());
        slot.set(std::ptr::from_mut(&mut session).cast());
    });
    let _reset = Reset;
    let status = call();
    assert_eq!(status == HARDWARE, session.captured.is_some());
    (status, session.captured)
}
