use super::super::window::{self, ClassKey, CreateParams, WS_POPUP};
use super::*;
use crate::{Win32, util::last_error};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Resume, RunResult, Wait, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn env(m: &mut Machine<JitCpu>, context: u32) -> Env<'_> {
    Env {
        context,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn drain(env: &mut Env<'_>, mut outcome: Outcome) -> u32 {
    loop {
        outcome = match outcome {
            Outcome::Return(value) => return value,
            Outcome::Call { args, then, .. } => {
                let value = u32::from(args.get(1) == Some(&0x81)); // WM_NCCREATE
                then(env, Resume::Returned(value))
            }
            _ => panic!("fixture window lifecycle"),
        };
    }
}
fn create(env: &mut Env<'_>, style: u32, parent: u32) -> u32 {
    let params = CreateParams {
        ex_style: 0,
        class: ClassKey::Name("STATIC".into()),
        class_ptr: 0,
        name_ptr: 0,
        text: String::new(),
        style,
        x: 0,
        y: 0,
        w: 100,
        h: 100,
        parent,
        menu: 0,
        instance: 0,
        param: 0,
    };
    let outcome = window::create_window(env, params, |_, hwnd| Outcome::Return(hwnd));
    let hwnd = drain(env, outcome);
    assert_ne!(hwnd, 0);
    hwnd
}
fn fixture(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    registry.add("test", "other_pause", Conv::Stdcall { argc: 0 }, |e| {
        Outcome::Block {
            wait: Wait::Until(e.sys.clock.now_us() + 1_000_000),
            then: Box::new(|_, _| Outcome::ExitThread(0)),
            allow_time_skip: true,
        }
    });
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/window-enumeration-tests");
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA + 2048);
    m.spawn("caller", CODE, &[], 65536).unwrap();
    let pause = m.resolve_import("test", "other_pause").unwrap();
    let mut other = Asm::new(CODE + 768);
    other.call(pause).hlt();
    m.cpu.write(CODE + 768, &other.bytes).unwrap();
    m.spawn("other owner", CODE + 768, &[], 65536).unwrap();
    m
}

struct Probe {
    seen: Vec<u32>,
    destroy: u32,
    reparent: u32,
}

#[test]
fn thread_window_callbacks_filter_snapshot_and_survive_nested_waits() {
    for jit in [false, true] {
        for stop in [false, true] {
            let mut m = fixture(jit);
            let root = create(&mut env(&mut m, 0), WS_POPUP, 0);
            let child = create(&mut env(&mut m, 0), WS_CHILD, root);
            let owned = create(&mut env(&mut m, 0), WS_POPUP, root);
            let destroyed = create(&mut env(&mut m, 0), WS_POPUP, 0);
            let reparented = create(&mut env(&mut m, 0), WS_POPUP, 0);
            let other = create(&mut env(&mut m, 1), WS_POPUP, 0);
            m.ext.get_mut::<Win32>().user.top = vec![owned, destroyed, reparented, root, other];
            m.ext.insert(Probe {
                seen: vec![],
                destroy: destroyed,
                reparent: reparented,
            });
            let registry = m.sys.thunks.resolver_mut::<Registry>().unwrap();
            registry.add("test", "probe", Conv::Stdcall { argc: 2 }, |e| {
                assert_eq!(e.context, 0); // Callback executes on the caller.
                assert_eq!(e.arg(1), 0xfedcba98);
                let hwnd = e.arg(0);
                let first = e.ext.get::<Probe>().seen.is_empty();
                e.ext.get_mut::<Probe>().seen.push(hwnd);
                if first {
                    let destroy = e.ext.get::<Probe>().destroy;
                    let reparent = e.ext.get::<Probe>().reparent;
                    let outcome =
                        window::destroy_window(e, destroy, |_, ok| Outcome::Return(u32::from(ok)));
                    assert_eq!(drain(e, outcome), 1);
                    e.ext
                        .get::<Win32>()
                        .user
                        .windows
                        .get_mut(&mut e.sys.objects, &reparent)
                        .unwrap()
                        .style |= WS_CHILD;
                    create(e, WS_POPUP, 0); // New windows are not in the outer snapshot.
                }
                set_last_error(e, 777);
                Outcome::Return(1)
            });
            registry.add("test", "wait", Conv::Stdcall { argc: 0 }, |e| {
                Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 1000), 0)
            });
            registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
                Outcome::ExitProcess(0)
            });
            let api = m.resolve_import("user32.dll", "EnumThreadWindows").unwrap();
            let probe = m.resolve_import("test", "probe").unwrap();
            let wait = m.resolve_import("test", "wait").unwrap();
            let done = m.resolve_import("test", "done").unwrap();
            let callback = CODE + 256;
            let inner_callback = CODE + 512;
            let tid = m.sys.context(0).tid;
            let mut main = Asm::new(CODE);
            main.bytes.extend_from_slice(&[0x89, 0xe0]);
            main.mov_mem_eax(DATA + 20)
                .push_imm(0xfedcba98)
                .push_imm(callback)
                .push_imm(tid)
                .call(api)
                .mov_mem_eax(DATA + 12);
            main.bytes.extend_from_slice(&[0x89, 0xe0]);
            main.mov_mem_eax(DATA + 24).call(done).hlt();
            m.cpu.write(CODE, &main.bytes).unwrap();
            let mut cb = Asm::new(callback);
            cb.mov_eax_esp(8)
                .push_eax()
                .mov_eax_esp(8)
                .push_eax()
                .call(probe)
                .push_imm(0)
                .push_imm(inner_callback)
                .push_imm(tid)
                .call(api)
                .call(wait);
            cb.bytes.push(0xb8); // mov eax, immediate
            cb.bytes.extend_from_slice(&u32::from(!stop).to_le_bytes());
            cb.ret_n(8);
            m.cpu.write(callback, &cb.bytes).unwrap();
            let mut inner = Asm::new(inner_callback);
            inner.call(wait);
            inner.bytes.extend_from_slice(&[0x31, 0xc0]); // xor eax, eax
            inner.ret_n(8);
            m.cpu.write(inner_callback, &inner.bytes).unwrap();
            loop {
                match m.run() {
                    RunResult::Exited(_) => break,
                    RunResult::Idle {
                        next_deadline: Some(t),
                    } => m.sys.clock.advance_to(t),
                    other => panic!("guest enumeration: {other:?}"),
                }
            }
            assert_eq!(
                m.ext.get::<Probe>().seen,
                if stop { vec![owned] } else { vec![owned, root] }
            );
            assert!(!m.ext.get::<Probe>().seen.contains(&child));
            assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), u32::from(!stop));
            assert_eq!(
                m.cpu.read_u32(DATA + 20).unwrap(),
                m.cpu.read_u32(DATA + 24).unwrap()
            );
            assert_eq!(last_error(&env(&mut m, 0)), 777);
        }
    }
}

#[test]
fn thread_window_empty_unknown_thread_and_invalid_callback_contract() {
    let mut m = fixture(false);
    let tid = m.sys.context(0).tid;
    set_last_error(&mut env(&mut m, 0), 1234);
    assert!(matches!(
        enumerate(&mut env(&mut m, 0), tid, GuestPtr::new(0), 0),
        Outcome::Return(1)
    ));
    assert_eq!(last_error(&env(&mut m, 0)), 1234);
    assert!(matches!(
        enumerate(&mut env(&mut m, 0), u32::MAX, GuestPtr::new(0), 0),
        Outcome::Return(1)
    ));
    assert_eq!(last_error(&env(&mut m, 0)), 87);
    let other = create(&mut env(&mut m, 1), WS_POPUP, 0);
    let Outcome::Call { args, then, .. } =
        enumerate(&mut env(&mut m, 0), 0, GuestPtr::new(CODE), -1)
    else {
        panic!("zero selects all guest threads")
    };
    assert_eq!(args, vec![other, u32::MAX]);
    assert!(matches!(
        then(&mut env(&mut m, 0), Resume::Returned(1)),
        Outcome::Return(1)
    ));
    for callback in [0, DATA, 0xdeadbeef] {
        assert!(matches!(
            enumerate(&mut env(&mut m, 0), 0, GuestPtr::new(callback), 0),
            Outcome::Return(0)
        ));
    }
}

#[test]
#[cfg(windows)]
fn thread_window_native_hidden_owned_child_empty_and_stop_contract() {
    use core::ffi::c_void;
    type Callback = unsafe extern "system" fn(*mut c_void, isize) -> i32;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: *mut c_void,
            menu: *mut c_void,
            instance: *mut c_void,
            param: *mut c_void,
        ) -> *mut c_void;
        fn DestroyWindow(hwnd: *mut c_void) -> i32;
        fn EnumThreadWindows(tid: u32, callback: Callback, parameter: isize) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn SetLastError(error: u32);
        fn GetLastError() -> u32;
    }
    struct NativeProbe {
        handles: Vec<usize>,
        stop: bool,
    }
    unsafe extern "system" fn callback(hwnd: *mut c_void, parameter: isize) -> i32 {
        let probe = unsafe { &mut *(parameter as *mut NativeProbe) };
        probe.handles.push(hwnd as usize);
        i32::from(!probe.stop)
    }
    std::thread::spawn(|| unsafe {
        let tid = GetCurrentThreadId();
        let mut probe = NativeProbe {
            handles: vec![],
            stop: false,
        };
        let p = &mut probe as *mut NativeProbe as isize;
        SetLastError(1234);
        assert_eq!(EnumThreadWindows(tid, callback, p), 1);
        assert!(probe.handles.is_empty());
        assert_eq!(GetLastError(), 1234);
        assert_eq!(EnumThreadWindows(u32::MAX, callback, p), 1);
        assert_eq!(GetLastError(), 87);
        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let create = |style, parent| {
            CreateWindowExW(
                0,
                class.as_ptr(),
                core::ptr::null(),
                style,
                0,
                0,
                100,
                100,
                parent,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
        let root = create(WS_POPUP, core::ptr::null_mut());
        let owned = create(WS_POPUP, root);
        let child = create(WS_CHILD, root);
        assert!(!root.is_null() && !owned.is_null() && !child.is_null());
        assert_eq!(EnumThreadWindows(tid, callback, p), 1);
        assert!(probe.handles.contains(&(root as usize)));
        assert!(probe.handles.contains(&(owned as usize)));
        assert!(!probe.handles.contains(&(child as usize)));
        probe.handles.clear();
        probe.stop = true;
        SetLastError(1234);
        assert_eq!(EnumThreadWindows(tid, callback, p), 0);
        assert_eq!(probe.handles.len(), 1);
        assert_eq!(GetLastError(), 1234);
        assert_ne!(DestroyWindow(owned), 0);
        assert_ne!(DestroyWindow(root), 0);
    })
    .join()
    .unwrap();
}
