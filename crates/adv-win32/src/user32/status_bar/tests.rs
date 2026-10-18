use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Reg},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

fn fixture() -> (Machine<JitCpu>, u32, u32) {
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
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/status-bar-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&root, "status.exe", &root)).unwrap();
    m.spawn("status owner", PARK_ADDR, &[], 65536).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    let u = &m.ext.get::<Win32>().user;
    let class = u.find_class(&ClassKey::Name(CLASS.into()), 0).unwrap();
    let base = u.classes[class].proc;
    assert_ne!(base, u.procs.def_window);
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
    w.unicode = true;
    w.rect = Rect::new(0, 578, 800, HEIGHT);
    (m, data, hwnd)
}

fn send(m: &mut Machine<JitCpu>, data: u32, hwnd: u32, msg: u32, wp: u32, lp: u32) -> u32 {
    m.cpu.set_reg(Reg::Esp, data + 256);
    for (i, a) in [hwnd, msg, wp, lp].into_iter().enumerate() {
        m.cpu.write_u32(data + 260 + i as u32 * 4, a).unwrap();
    }
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let Outcome::Return(v) = proc(&mut env) else {
        panic!("status bar state must return synchronously")
    };
    v
}

fn wide(m: &mut Machine<JitCpu>, at: u32, text: &str) {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(at, &bytes).unwrap();
}

fn read_wide(m: &Machine<JitCpu>, at: u32, units: usize) -> String {
    let mut bytes = vec![0; units * 2];
    m.cpu.read(at, &mut bytes).unwrap();
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn status(m: &Machine<JitCpu>, hwnd: u32) -> (String, String) {
    let w = m
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&m.sys.objects, &hwnd)
        .unwrap();
    (
        w.text.clone(),
        w.control.status.as_ref().unwrap().display(&w.text),
    )
}

// spec: WIN32-USER-17
#[test]
fn bars_align_to_the_parent_edge_unless_told_not_to() {
    let any = Rect::new(5, 40, 10, 10);
    assert_eq!(
        placement(0, any, (800, 600), HEIGHT),
        Some(Rect::new(0, 578, 800, HEIGHT))
    );
    assert_eq!(
        placement(CCS_TOP, any, (800, 600), HEIGHT),
        Some(Rect::new(0, 0, 800, HEIGHT))
    );
    assert_eq!(
        placement(CCS_NOPARENTALIGN, any, (640, 480), HEIGHT),
        Some(Rect::new(0, 40, 640, HEIGHT))
    );
    assert_eq!(placement(CCS_NORESIZE, any, (800, 600), HEIGHT), None);
}

// spec: WIN32-USER-17
#[test]
fn parts_text_types_and_simple_mode_round_trip() {
    let (mut m, data, hwnd) = fixture();
    let (text, edges, out) = (data + 1024, data + 2048, data + 3072);
    // Three parts: [0, 200), [200, 500), [500, right edge).
    for (i, edge) in [200i32, 500, -1].into_iter().enumerate() {
        m.cpu
            .write(edges + i as u32 * 4, &edge.to_le_bytes())
            .unwrap();
    }
    assert_eq!(send(&mut m, data, hwnd, 0x404, 3, edges), 1);
    assert_eq!(send(&mut m, data, hwnd, 0x406, 0, 0), 3);
    assert_eq!(send(&mut m, data, hwnd, 0x404, 0, edges), 0);

    wide(&mut m, text, "準備完了");
    assert_eq!(send(&mut m, data, hwnd, 0x40b, 0, text), 1);
    wide(&mut m, text, "1920x1080");
    assert_eq!(send(&mut m, data, hwnd, 0x40b, 2 | 0x200, text), 1);
    assert_eq!(
        status(&m, hwnd),
        ("準備完了".into(), "準備完了    1920x1080".into())
    );
    // SB_GETTEXTW and SB_GETTEXTLENGTHW: MAKELONG(length, type).
    assert_eq!(send(&mut m, data, hwnd, 0x40d, 2, out), 9 | 0x200 << 16);
    assert_eq!(read_wide(&m, out, 9), "1920x1080");
    assert_eq!(send(&mut m, data, hwnd, 0x40c, 0, 0), 4);
    assert_eq!(send(&mut m, data, hwnd, 0x40c, 1, 0), 0);

    // SB_GETRECT follows the part edges and the bar's height.
    assert_eq!(send(&mut m, data, hwnd, 0x40a, 1, out), 1);
    let mut rect = [0u8; 16];
    m.cpu.read(out, &mut rect).unwrap();
    let rect: Vec<i32> = rect
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes(*b))
        .collect();
    assert_eq!(rect, [202, 2, 500, HEIGHT - 2]);
    assert_eq!(send(&mut m, data, hwnd, 0x40a, 3, out), 0);

    // Simple mode shows only its own text and keeps the parts.
    wide(&mut m, text, "読み込み中");
    assert_eq!(send(&mut m, data, hwnd, 0x40b, 255, text), 1);
    assert_eq!(send(&mut m, data, hwnd, 0x409, 1, 0), 0);
    assert_eq!(send(&mut m, data, hwnd, 0x40e, 0, 0), 1);
    assert_eq!(status(&m, hwnd).1, "読み込み中");
    assert_eq!(send(&mut m, data, hwnd, 0x409, 0, 0), 0);
    assert_eq!(status(&m, hwnd).1, "準備完了    1920x1080");
}
