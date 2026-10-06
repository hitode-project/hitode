use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Reg, asm::Asm},
};
use adv_machine::{Clock, Machine, Resume, RunResult, Wait, layout::PARK_ADDR};

fn fixture() -> (Machine<JitCpu>, u32, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tab-control-tests");
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, data + 256);
    let u = &m.ext.get::<Win32>().user;
    let class = u
        .find_class(
            &super::super::window::ClassKey::Name("SysTabControl32".into()),
            0,
        )
        .unwrap();
    let base = u.classes[class].proc;
    assert_ne!(base, u.procs.def_window);
    assert_eq!(u.classes[class].wnd_extra, 4);
    let hwnd = u.desktop;
    let w = m
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &hwnd)
        .unwrap();
    w.class = class;
    w.proc = base;
    w.thread = m.sys.current;
    (m, data, hwnd, base)
}
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn send(m: &mut Machine<JitCpu>, data: u32, hwnd: u32, msg: u32, index: i32, pointer: u32) -> i32 {
    m.cpu.set_reg(Reg::Esp, data + 256);
    for (i, a) in [hwnd, msg, index as u32, pointer].into_iter().enumerate() {
        m.cpu.write_u32(data + 260 + i as u32 * 4, a).unwrap();
    }
    let Outcome::Return(v) = proc(&mut env(m)) else {
        panic!("tab state must return synchronously")
    };
    v as i32
}
fn put(m: &mut Machine<JitCpu>, pointer: u32, values: [u32; 7]) {
    GuestPtr::new(pointer).write(&mut m.cpu, values).unwrap();
}

#[test]
fn tab_item_masks_text_encodings_and_protected_outputs_share_window_lifetime() {
    let (mut m, data, hwnd, _) = fixture();
    m.spawn("tab owner", PARK_ADDR, &[], 65536).unwrap();
    let item = data + 512;
    let text = data + 640;
    let output = data + 768;
    m.cpu.write(text, b"One\0").unwrap();
    put(&mut m, item, [1 | 2 | 8, 0, 0, text, 0, 7, 0x12345678]);
    assert_eq!(send(&mut m, data, hwnd, 0x1307, 0, item), 0);
    assert_eq!(send(&mut m, data, hwnd, 0x130b, 0, 0), 0);
    let wide = "猫"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    m.cpu.write(text, &wide).unwrap();
    put(&mut m, item, [1, 0, 0, text, 0, 0, 0]);
    assert_eq!(send(&mut m, data, hwnd, 0x133d, 0, item), 1);
    put(&mut m, item, [1 | 2 | 8 | 16, 0, 3, output, 16, 0, 0]);
    assert_eq!(send(&mut m, data, hwnd, 0x133c, 0, item), 1);
    let result = GuestPtr::<[u32; 7]>::new(item).read(&m.cpu).unwrap();
    assert_eq!((result[1], result[5], result[6]), (1, 7, 0x12345678));
    let actual = WideStr(GuestPtr::new(output)).read(&m.cpu, 16).unwrap();
    assert_eq!(actual, vec!['猫' as u16]);
    put(&mut m, item, [1, 0, 0, output, 16, 0, 0]);
    assert_eq!(send(&mut m, data, hwnd, 0x1305, 0, item), 1);
    assert_eq!(
        AnsiStr(GuestPtr::new(output)).read(&m.cpu, 16).unwrap(),
        m.ext.get::<Win32>().codec.encode("猫")
    );
    assert_eq!(send(&mut m, data, hwnd, 0x1306, 99, item), 0);
    assert_eq!(send(&mut m, data, hwnd, 0x1307, -1, item), -1);
    put(&mut m, item, [8, 0, 0, 0xdeadbeef, 0, 0, 77]);
    assert_eq!(send(&mut m, data, hwnd, 0x1306, 0, item), 1); // unrequested text is not read
    let boundary = data + 4096 - 24;
    put(
        &mut m,
        boundary,
        [8 | 16, 0xcccccccc, 3, 0, 0, 0, 0xaaaaaaaa],
    );
    m.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(send(&mut m, data, hwnd, 0x1305, 0, boundary), 0);
    assert_eq!(m.cpu.read_u32(boundary + 4).unwrap(), 0xcccccccc);
    let mut outcome = super::super::window::destroy_window(&mut env(&mut m), hwnd, |_, ok| {
        Outcome::Return(u32::from(ok))
    });
    loop {
        outcome = match outcome {
            Outcome::Call { then, .. } => then(&mut env(&mut m), Resume::Returned(0)),
            Outcome::Return(1) => break,
            _ => panic!("window destruction"),
        };
    }
    assert!(
        !m.ext
            .get::<Win32>()
            .user
            .windows
            .contains_key(&m.sys.objects, &hwnd)
    );
    assert_eq!(send(&mut m, data, hwnd, 0x1304, 0, 0), 0);
}

#[test]
fn tab_saved_system_proc_is_reachable_from_guest_subclass_after_waits() {
    let (mut m, data, hwnd, base) = fixture();
    let code = 0x400000;
    m.cpu.map(code, 4096, Prot::RX).unwrap();
    let callback = code + 256;
    m.ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &hwnd)
        .unwrap()
        .proc = callback;
    let send = m.resolve_import("user32.dll", "SendMessageA").unwrap();
    let call = m.resolve_import("user32.dll", "CallWindowProcA").unwrap();
    let registry = m.sys.thunks.resolver_mut::<Registry>().unwrap();
    registry.add("test", "wait", adv_abi::Conv::Stdcall { argc: 0 }, |e| {
        Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 1000), 0)
    });
    registry.add("test", "done", adv_abi::Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitThread(0)
    });
    let wait = m.resolve_import("test", "wait").unwrap();
    let done = m.resolve_import("test", "done").unwrap();
    put(&mut m, data + 512, [8, 0, 0, 0, 0, 0, 0x12345678]);
    let mut main = Asm::new(code);
    main.push_imm(data + 512)
        .push_imm(0)
        .push_imm(0x1307)
        .push_imm(hwnd)
        .call(send)
        .mov_mem_eax(data + 64)
        .push_imm(0)
        .push_imm(0)
        .push_imm(0x1304)
        .push_imm(hwnd)
        .call(send)
        .mov_mem_eax(data + 68)
        .call(done)
        .hlt();
    m.cpu.write(code, &main.bytes).unwrap();
    let mut cb = Asm::new(callback);
    cb.call(wait)
        .mov_eax_esp(16)
        .push_eax()
        .mov_eax_esp(16)
        .push_eax()
        .mov_eax_esp(16)
        .push_eax()
        .mov_eax_esp(16)
        .push_eax()
        .push_imm(base)
        .call(call)
        .ret_n(16);
    m.cpu.write(callback, &cb.bytes).unwrap();
    m.spawn("tab owner", code, &[], 65536).unwrap();
    loop {
        match m.run() {
            RunResult::Exited(_) => break,
            RunResult::Idle {
                next_deadline: Some(t),
            } => m.sys.clock.advance_to(t),
            other => panic!("guest tab callback: {other:?}"),
        }
    }
    assert_eq!(m.cpu.read_u32(data + 64).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(data + 68).unwrap(), 1);
    assert_eq!(state(&mut env(&mut m), hwnd).items[0].parameter, 0x12345678);
}

#[test]
fn tab_adjust_rect_validates_the_whole_rectangle_and_preserves_round_trip() {
    let (mut m, data, hwnd, _) = fixture();
    let pointer = data + 768;
    let rect = [3i32, 4, 503, 204];
    GuestPtr::new(pointer).write(&mut m.cpu, rect).unwrap();
    assert_eq!(send(&mut m, data, hwnd, 0x1328, 0, pointer), 0);
    assert_eq!(
        GuestPtr::<[i32; 4]>::new(pointer).read(&m.cpu).unwrap(),
        [7, 8, 499, 200]
    );
    assert_eq!(send(&mut m, data, hwnd, 0x1328, 1, pointer), 0);
    assert_eq!(
        GuestPtr::<[i32; 4]>::new(pointer).read(&m.cpu).unwrap(),
        rect
    );
    GuestPtr::new(data + 4096).write(&mut m.cpu, rect).unwrap();
    m.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    send(&mut m, data, hwnd, 0x1328, 0, data + 4096);
    assert_eq!(
        GuestPtr::<[i32; 4]>::new(data + 4096).read(&m.cpu).unwrap(),
        rect
    );
    GuestPtr::new(pointer)
        .write(&mut m.cpu, [i32::MIN, 0, 100, 100])
        .unwrap();
    send(&mut m, data, hwnd, 0x1328, 1, pointer);
    assert_eq!(m.cpu.read_u32(pointer).unwrap(), i32::MIN as u32);
}

#[test]
#[cfg(windows)]
fn tab_adjust_rect_matches_native_font_height_and_single_row_profile() {
    use core::ffi::c_void;
    #[repr(C)]
    struct NativeItem {
        mask: u32,
        state: u32,
        state_mask: u32,
        text: *mut u16,
        capacity: i32,
        image: i32,
        parameter: isize,
    }
    #[link(name = "comctl32")]
    unsafe extern "system" {
        fn InitCommonControls();
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: *mut c_void,
            menu: *mut c_void,
            instance: *mut c_void,
            param: *mut c_void,
        ) -> *mut c_void;
        fn SendMessageW(hwnd: *mut c_void, msg: u32, wp: usize, lp: isize) -> isize;
        fn DestroyWindow(hwnd: *mut c_void) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateFontIndirectA(logfont: *const u8) -> *mut c_void;
        fn DeleteObject(font: *mut c_void) -> i32;
        fn GetStockObject(index: i32) -> *mut c_void;
    }
    let class = "SysTabControl32"
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
    // SAFETY: hidden authored control and fonts are owned by the fixture; each
    // native send receives a complete aligned structure and returns synchronously.
    unsafe {
        InitCommonControls();
    }
    for height in [None, Some(-8i32), Some(-24), Some(-40)] {
        let native = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                [0u16].as_ptr(),
                0,
                0,
                0,
                500,
                200,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
        assert!(!native.is_null());
        let (mut m, data, hwnd, _) = fixture();
        let pointer = data + 768;
        let mut native_font = core::ptr::null_mut();
        if let Some(height) = height {
            let mut logfont = [0; 60];
            logfont[..4].copy_from_slice(&height.to_le_bytes());
            logfont[16..20].copy_from_slice(&400i32.to_le_bytes());
            logfont[28..34].copy_from_slice(b"Arial\0");
            native_font = unsafe { CreateFontIndirectA(logfont.as_ptr()) };
            assert!(!native_font.is_null());
            unsafe {
                SendMessageW(native, wm::SETFONT, native_font as usize, 0);
            }
            m.cpu.write(data + 1024, &logfont).unwrap();
            m.cpu.write_u32(data + 260, data + 1024).unwrap();
            let address = m
                .resolve_import("gdi32.dll", "CreateFontIndirectA")
                .unwrap();
            let index =
                (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            let handler = m.sys.thunks.get(index).f.handler;
            let Outcome::Return(font) = handler(&mut env(&mut m)) else {
                panic!("font creation")
            };
            assert_ne!(font, 0);
            send(&mut m, data, hwnd, wm::SETFONT, font as i32, 0);
        } else {
            // Match the virtual profile explicitly. Native controls may pick
            // the host message font, which varies with theme and process DPI.
            unsafe {
                SendMessageW(native, wm::SETFONT, GetStockObject(13) as usize, 0);
            }
        }
        for count in 0..3 {
            if count > 0 {
                let mut entry = NativeItem {
                    mask: 0,
                    state: 0,
                    state_mask: 0,
                    text: core::ptr::null_mut(),
                    capacity: 0,
                    image: 0,
                    parameter: 0,
                };
                unsafe {
                    SendMessageW(
                        native,
                        0x133e,
                        (count - 1) as usize,
                        &mut entry as *mut _ as isize,
                    );
                }
                put(&mut m, data + 512, [0; 7]);
                assert_eq!(
                    send(&mut m, data, hwnd, 0x133e, count - 1, data + 512),
                    count - 1
                );
            }
            assert_eq!(send(&mut m, data, hwnd, 0x132c, 0, 0), unsafe {
                SendMessageW(native, 0x132c, 0, 0)
            } as i32);
            for grow in [0, 1] {
                let mut rect = [3i32, 4, 503, 204];
                unsafe {
                    SendMessageW(native, 0x1328, grow as usize, rect.as_mut_ptr() as isize);
                }
                GuestPtr::new(pointer)
                    .write(&mut m.cpu, [3i32, 4, 503, 204])
                    .unwrap();
                send(&mut m, data, hwnd, 0x1328, grow, pointer);
                assert_eq!(
                    GuestPtr::<[i32; 4]>::new(pointer).read(&m.cpu).unwrap(),
                    rect,
                    "font={height:?} count={count} grow={grow}"
                );
            }
        }
        assert_eq!(unsafe { DestroyWindow(native) }, 1);
        if !native_font.is_null() {
            assert_eq!(unsafe { DeleteObject(native_font) }, 1);
        }
    }
}

#[test]
#[cfg(windows)]
fn tab_insert_parameter_selection_and_deletion_match_native_control() {
    use core::ffi::c_void;
    #[repr(C)]
    struct NativeItem {
        mask: u32,
        state: u32,
        state_mask: u32,
        text: *mut u16,
        capacity: i32,
        image: i32,
        parameter: isize,
    }
    #[link(name = "comctl32")]
    unsafe extern "system" {
        fn InitCommonControls();
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: *mut c_void,
            menu: *mut c_void,
            instance: *mut c_void,
            param: *mut c_void,
        ) -> *mut c_void;
        fn SendMessageW(hwnd: *mut c_void, msg: u32, wp: usize, lp: isize) -> isize;
        fn DestroyWindow(hwnd: *mut c_void) -> i32;
    }
    let class = "SysTabControl32"
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
    // SAFETY: authored hidden native control, owned until DestroyWindow. Item
    // storage is ABI-aligned and remains live during each synchronous send.
    unsafe {
        InitCommonControls();
    }
    let native = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            [0u16].as_ptr(),
            0,
            0,
            0,
            200,
            100,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };
    assert!(!native.is_null());
    let (mut m, data, hwnd, _) = fixture();
    let pointer = data + 512;
    let native_send =
        |msg, index, ptr| unsafe { SendMessageW(native, msg, index as usize, ptr) } as i32;
    assert_eq!(
        send(&mut m, data, hwnd, 0x130b, 0, 0),
        native_send(0x130b, 0, 0)
    );
    for (index, parameter) in [(0, 11), (0, 22), (999, 33), (-1, 44)] {
        let mut n = NativeItem {
            mask: 24,
            state: 3,
            state_mask: 3,
            text: core::ptr::null_mut(),
            capacity: 0,
            image: 0,
            parameter,
        };
        put(&mut m, pointer, [24, 3, 3, 0, 0, 0, parameter as u32]);
        assert_eq!(
            send(&mut m, data, hwnd, 0x133e, index, pointer),
            native_send(0x133e, index, &mut n as *mut _ as isize)
        );
        for msg in [0x1304, 0x130b] {
            assert_eq!(send(&mut m, data, hwnd, msg, 0, 0), native_send(msg, 0, 0));
        }
    }
    for index in 0..3 {
        let mut n = NativeItem {
            mask: 24,
            state: 0,
            state_mask: u32::MAX,
            text: core::ptr::null_mut(),
            capacity: 0,
            image: 0,
            parameter: 0,
        };
        put(&mut m, pointer, [24, 0, u32::MAX, 0, 0, 0, 0]);
        assert_eq!(
            send(&mut m, data, hwnd, 0x133c, index, pointer),
            native_send(0x133c, index, &mut n as *mut _ as isize)
        );
        let v = GuestPtr::<[u32; 7]>::new(pointer).read(&m.cpu).unwrap();
        assert_eq!((v[1], v[6]), (n.state, n.parameter as u32));
    }
    let mut n = NativeItem {
        mask: 8,
        state: 0,
        state_mask: 0,
        text: core::ptr::null_mut(),
        capacity: 0,
        image: 0,
        parameter: 55,
    };
    put(&mut m, pointer, [8, 0, 0, 0, 0, 0, 55]);
    assert_eq!(
        send(&mut m, data, hwnd, 0x133d, 0, pointer),
        native_send(0x133d, 0, &mut n as *mut _ as isize)
    );
    for index in [2, -1, 999, 0] {
        assert_eq!(
            send(&mut m, data, hwnd, 0x130c, index, 0),
            native_send(0x130c, index, 0)
        );
        assert_eq!(
            send(&mut m, data, hwnd, 0x130b, 0, 0),
            native_send(0x130b, 0, 0)
        );
    }
    for index in [0, 99, 0, 0, 0] {
        assert_eq!(
            send(&mut m, data, hwnd, 0x1308, index, 0),
            native_send(0x1308, index, 0)
        );
        for msg in [0x1304, 0x130b] {
            assert_eq!(send(&mut m, data, hwnd, msg, 0, 0), native_send(msg, 0, 0));
        }
    }
    assert_eq!(
        send(&mut m, data, hwnd, 0x1309, 0, 0),
        native_send(0x1309, 0, 0)
    );
    assert_eq!(unsafe { DestroyWindow(native) }, 1);
}
