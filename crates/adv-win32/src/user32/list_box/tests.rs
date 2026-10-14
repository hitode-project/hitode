use super::*;
use crate::user32::window::{ClassKey, Rect};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

fn fixture(style: u32) -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/list-box-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&root, "list.exe", &root)).unwrap();
    m.spawn("list owner", PARK_ADDR, &[], 65536).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    let u = &m.ext.get::<Win32>().user;
    let class = u.find_class(&ClassKey::Name("ListBox".into()), 0).unwrap();
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
    w.unicode = false;
    w.style = style;
    w.rect = Rect::new(0, 0, 280, 230);
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
        panic!("list box state must return synchronously")
    };
    v
}

fn string(m: &mut Machine<JitCpu>, data: u32, text: &str) -> u32 {
    let at = data + 1024;
    m.cpu.write(at, text.as_bytes()).unwrap();
    m.cpu.write(at + text.len() as u32, &[0]).unwrap();
    at
}

fn text(m: &mut Machine<JitCpu>, data: u32, hwnd: u32, index: u32) -> String {
    let at = data + 2048;
    let length = send(m, data, hwnd, 0x189, index, at);
    assert_eq!(send(m, data, hwnd, 0x18a, index, 0), length);
    let mut bytes = vec![0; length as usize];
    m.cpu.read(at, &mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
// spec: WIN32-USER-18
fn sorted_strings_keep_selection_find_and_item_data() {
    let (mut m, data, hwnd) = fixture(0x5000_0103);
    for name in ["SimSun", "Arial", "msgothic", "KaiTi"] {
        let at = string(&mut m, data, name);
        send(&mut m, data, hwnd, 0x180, 0, at);
    }
    assert_eq!(send(&mut m, data, hwnd, 0x18b, 0, 0), 4);
    let names: Vec<_> = (0..4).map(|i| text(&mut m, data, hwnd, i)).collect();
    assert_eq!(names, ["Arial", "KaiTi", "msgothic", "SimSun"]);
    let at = string(&mut m, data, "sim");
    assert_eq!(send(&mut m, data, hwnd, 0x18f, u32::MAX, at), 3);
    assert_eq!(send(&mut m, data, hwnd, 0x1a2, u32::MAX, at), LB_ERR);
    let at = string(&mut m, data, "KAITI");
    assert_eq!(send(&mut m, data, hwnd, 0x1a2, u32::MAX, at), 1);
    assert_eq!(send(&mut m, data, hwnd, 0x186, 2, 0), 2);
    assert_eq!(send(&mut m, data, hwnd, 0x187, 2, 0), 1);
    assert_eq!(send(&mut m, data, hwnd, 0x19a, 2, 0x1234), 0);
    // A sorted insertion before the selection moves it with its item.
    let at = string(&mut m, data, "Batang");
    assert_eq!(send(&mut m, data, hwnd, 0x180, 0, at), 1);
    assert_eq!(send(&mut m, data, hwnd, 0x188, 0, 0), 3);
    assert_eq!(text(&mut m, data, hwnd, 3), "msgothic");
    assert_eq!(send(&mut m, data, hwnd, 0x199, 3, 0), 0x1234);
    assert_eq!(send(&mut m, data, hwnd, 0x182, 0, 0), 4);
    assert_eq!(send(&mut m, data, hwnd, 0x188, 0, 0), 2);
    assert_eq!(send(&mut m, data, hwnd, 0x186, u32::MAX, 0), LB_ERR);
    assert_eq!(send(&mut m, data, hwnd, 0x188, 0, 0), LB_ERR);
    assert_eq!(send(&mut m, data, hwnd, 0x189, 9, data + 2048), LB_ERR);
    assert_eq!(send(&mut m, data, hwnd, 0x197, 3, 0), 0);
    assert_eq!(send(&mut m, data, hwnd, 0x18e, 0, 0), 3);
    assert_eq!(send(&mut m, data, hwnd, 0x184, 0, 0), 0);
    assert_eq!(send(&mut m, data, hwnd, 0x18b, 0, 0), 0);
}

#[test]
// spec: WIN32-USER-18
fn unsorted_lists_insert_in_place_and_multiple_selection_is_unavailable() {
    let (mut m, data, hwnd) = fixture(0x5000_0001);
    for name in ["b", "a"] {
        let at = string(&mut m, data, name);
        send(&mut m, data, hwnd, 0x180, 0, at);
    }
    let at = string(&mut m, data, "c");
    assert_eq!(send(&mut m, data, hwnd, 0x181, 1, at), 1);
    assert_eq!(send(&mut m, data, hwnd, 0x181, 9, at), LB_ERR);
    let names: Vec<_> = (0..3).map(|i| text(&mut m, data, hwnd, i)).collect();
    assert_eq!(names, ["b", "c", "a"]);
    assert!(projectable(0x5000_0001));
    assert!(!projectable(0x5000_0809));
    assert!(!projectable(0x5000_0011));
    assert!(projectable(0x5000_0051));
    let (mut m, data, hwnd) = fixture(0x5000_0809);
    assert_eq!(send(&mut m, data, hwnd, 0x186, 0, 0), LB_ERR);
}
