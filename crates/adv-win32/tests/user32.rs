//! USER32 integration tests: x86 guest calls resolved imports and records callbacks in
//! guest memory. No game files, host-side window injection, or mocked API handlers.

use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Machine, Registry, RunResult};
use adv_unicorn::UnicornCpu;
use adv_win32::user32::window::{
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_EX_NOPARENTNOTIFY, WS_POPUP, WS_THICKFRAME, WS_VISIBLE, wm,
};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const LOG: u32 = 0x0042_0000;
const LOG_NEXT: u32 = DATA;
const BM_GETCHECK: u32 = 0xf0;
const WS_GROUP: u32 = 0x0002_0000;
const APP: u32 = 0x8000;

#[derive(Clone, Copy)]
enum Arg {
    Imm(u32),
    Mem(u32),
    Stack(u8),
}
use Arg::{Imm as I, Mem as M, Stack as S};

fn store(a: &mut Asm, addr: u32) {
    a.bytes.push(0xa3); // mov [addr], eax
    a.bytes.extend_from_slice(&addr.to_le_bytes());
}

fn value(a: &mut Asm, v: u32) {
    a.bytes.push(0xb8); // mov eax, imm32
    a.bytes.extend_from_slice(&v.to_le_bytes());
}

fn invoke(a: &mut Asm, proc: u32, args: &[Arg]) {
    for (n, arg) in args.iter().rev().enumerate() {
        match arg {
            I(v) => {
                a.push_imm(*v);
            }
            M(p) => {
                a.mov_eax_mem(*p).push_eax();
            }
            S(off) => {
                a.mov_eax_esp(*off + 4 * n as u8).push_eax();
            }
        }
    }
    a.call(proc);
}

fn on_message(a: &mut Asm, msg: u32, body: impl FnOnce(&mut Asm)) {
    a.mov_eax_esp(8);
    a.bytes.push(0x3d); // cmp eax, msg
    a.bytes.extend_from_slice(&msg.to_le_bytes());
    let patch = a.bytes.len() + 2;
    let next = a.here() + 6;
    a.jnz(0);
    body(a);
    let delta = a.here().wrapping_sub(next);
    a.bytes[patch..patch + 4].copy_from_slice(&delta.to_le_bytes());
}

/// Only caller-saved registers are touched; the original four stdcall arguments stay put.
fn record(a: &mut Asm) {
    a.bytes.extend_from_slice(&[0x8b, 0x15]); // mov edx, [LOG_NEXT]
    a.bytes.extend_from_slice(&LOG_NEXT.to_le_bytes());
    for i in 0..4 {
        a.mov_eax_esp(4 + i * 4);
        a.bytes.extend_from_slice(&[0x89, 0x42, i * 4]); // mov [edx + i*4], eax
    }
    a.bytes.extend_from_slice(&[0x83, 0xc2, 16, 0x89, 0x15]);
    a.bytes.extend_from_slice(&LOG_NEXT.to_le_bytes());
}

fn word(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn dword(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn wide(bytes: &mut Vec<u8>, value: &str) {
    for ch in value.encode_utf16().chain([0]) {
        word(bytes, ch);
    }
}

/// Hand-built, DWORD-aligned standard/extended templates, including font and item data.
fn dialog_template(extended: bool, custom_class: Option<&str>) -> Vec<u8> {
    let mut b = Vec::new();
    let style = WS_POPUP | 0x40; // DS_SETFONT
    if extended {
        word(&mut b, 1);
        word(&mut b, 0xffff);
        dword(&mut b, 0); // helpID
        dword(&mut b, 0); // exStyle
        dword(&mut b, style);
    } else {
        dword(&mut b, style);
        dword(&mut b, 0);
    }
    for v in [2, 2, 3, 100, 60, 0, 0] {
        // count, x, y, cx, cy, menu, class
        word(&mut b, v);
    }
    wide(&mut b, "Guest dialog");
    word(&mut b, 9);
    if extended {
        word(&mut b, 400);
        b.extend_from_slice(&[0, 1]); // italic, charset
    }
    wide(&mut b, "Tahoma");
    for i in 0..2 {
        b.resize(b.len().next_multiple_of(4), 0);
        let style = WS_CHILD | WS_VISIBLE | 0x1_0000 | i; // tab stop; second button is default
        if extended {
            dword(&mut b, 0);
            dword(&mut b, 0);
            dword(&mut b, style);
        } else {
            dword(&mut b, style);
            dword(&mut b, 0);
        }
        for v in [5, 5 + 20 * i as u16, 40, 15] {
            word(&mut b, v);
        }
        if extended {
            dword(&mut b, 0x1_0000 + 41 + i);
        } else {
            word(&mut b, 41 + i as u16);
        }
        if let Some(class) = custom_class.filter(|_| i == 0) {
            wide(&mut b, class);
        } else {
            word(&mut b, 0xffff);
            word(&mut b, 0x80); // BUTTON ordinal
        }
        wide(&mut b, if i == 0 { "First" } else { "OK" });
        if i == 0 && custom_class.is_some() {
            // Native USER32 counts bytes following the size word in both formats,
            // and passes a pointer to the size word as CREATESTRUCT.lpCreateParams.
            word(&mut b, 4);
            word(&mut b, 0xabcd);
            word(&mut b, 0xef01);
        } else {
            word(&mut b, 0);
        }
    }
    b
}

struct T {
    m: Machine<UnicornCpu>,
    data: u32,
    proc: u32,
}

#[test]
fn posting_to_a_live_worker_succeeds_but_an_exited_thread_id_fails() {
    let mut t = T::new();
    let worker = CODE + 0x8000;
    let message = t.alloc(28);
    let mut body = Asm::new(worker);
    t.call(&mut body, "GetMessageA", &[I(message), I(0), I(0), I(0)]);
    value(&mut body, 7);
    body.ret_n(4);
    t.m.cpu.write(worker, &body.bytes).unwrap();

    let tid = t.alloc(4);
    let mut main = Asm::new(CODE);
    let thread = t.call(
        &mut main,
        "kernel32.dll!CreateThread",
        &[I(0), I(0), I(worker), I(0), I(0), I(tid)],
    );
    // Let the worker create its queue and block in a genuine guest GetMessage.
    t.call(&mut main, "kernel32.dll!Sleep", &[I(0)]);
    let live = t.call(
        &mut main,
        "PostThreadMessageA",
        &[M(tid), I(APP + 42), I(123), I(456)],
    );
    t.call(
        &mut main,
        "kernel32.dll!WaitForSingleObject",
        &[M(thread), I(u32::MAX)],
    );
    let exit = t.alloc(4);
    t.call(
        &mut main,
        "kernel32.dll!GetExitCodeThread",
        &[M(thread), I(exit)],
    );
    let dead = t.call(
        &mut main,
        "PostThreadMessageA",
        &[M(tid), I(APP + 43), I(789), I(0)],
    );
    let dead_error = t.call(&mut main, "kernel32.dll!GetLastError", &[]);
    let missing = t.call(
        &mut main,
        "PostThreadMessageA",
        &[I(u32::MAX), I(APP + 44), I(0), I(0)],
    );
    let missing_error = t.call(&mut main, "kernel32.dll!GetLastError", &[]);
    t.run(main);

    assert_eq!(t.u32(live), 1);
    assert_eq!(t.words::<4>(message), [0, APP + 42, 123, 456]);
    assert_eq!(t.u32(exit), 7);
    assert_eq!(t.u32(dead), 0);
    assert_eq!(t.u32(dead_error), 1444); // ERROR_INVALID_THREAD_ID
    assert_eq!(t.u32(missing), 0);
    assert_eq!(t.u32(missing_error), 1444);
    assert!(t.m.sys.contexts[1].is_done());
    assert!(t.m.ext.get::<Win32>().user.queues[&1].posted.is_empty());
}

impl T {
    fn new() -> Self {
        Self::with_auto_dialog(None)
    }

    fn with_auto_dialog(auto_dialog_ms: Option<u32>) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let cpu = UnicornCpu::new(PARK_ADDR).unwrap();
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        m.cpu.map(CODE, 0x1_0000, Prot::RX).unwrap();
        m.cpu.map(DATA, 0x2_0000, Prot::RW).unwrap();
        m.cpu.write_u32(LOG_NEXT, LOG).unwrap();
        // USER32 does not access these paths; keep them independent of installed games.
        let root = std::env::temp_dir().join("pal-user32-no-files");
        let mut cfg = Win32Config::new(root.join("game"), "user32-test.exe", root.join("data"));
        cfg.auto_dialog_ms = auto_dialog_ms;
        Win32::install(&mut m, cfg).unwrap();
        Self {
            m,
            data: DATA + 0x100,
            proc: CODE + 0x8000,
        }
    }

    fn alloc(&mut self, size: u32) -> u32 {
        let p = self.data;
        self.data += size.next_multiple_of(4);
        assert!(self.data < LOG);
        p
    }

    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let p = self.alloc(bytes.len() as u32);
        self.m.cpu.write(p, bytes).unwrap();
        p
    }

    fn text(&mut self, text: &str) -> u32 {
        self.bytes(&[text.as_bytes(), &[0]].concat())
    }

    fn f(&mut self, name: &str) -> u32 {
        let (dll, name) = name.split_once('!').unwrap_or(("user32.dll", name));
        self.m.resolve_import(dll, name).unwrap()
    }

    fn call(&mut self, a: &mut Asm, name: &str, args: &[Arg]) -> u32 {
        invoke(a, self.f(name), args);
        let out = self.alloc(4);
        store(a, out);
        out
    }

    fn wndproc(&mut self, body: impl FnOnce(&mut Asm)) -> u32 {
        let addr = self.proc;
        let mut a = Asm::new(addr);
        record(&mut a);
        body(&mut a);
        a.jmp(self.f("DefWindowProcA"));
        self.proc = a.here().next_multiple_of(16);
        assert!(self.proc < CODE + 0x1_0000);
        self.m.cpu.write(addr, &a.bytes).unwrap();
        addr
    }

    fn class(&mut self, a: &mut Asm, proc: u32) -> u32 {
        let name = self.text(&format!("User32Test{proc:x}"));
        let fields = [0, proc, 0, 0, CODE, 0, 0, 0, 0, name];
        let wc = self.bytes(
            &fields
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        self.call(a, "RegisterClassA", &[I(wc)]);
        name
    }

    fn window(&mut self, a: &mut Asm, class: u32, style: u32, parent: Arg, id: u32) -> u32 {
        self.call(
            a,
            "CreateWindowExA",
            &[
                I(0),
                I(class),
                I(0),
                I(style),
                I(20),
                I(30),
                I(120),
                I(80),
                parent,
                I(id),
                I(CODE),
                I(0),
            ],
        )
    }

    fn start(&mut self, mut a: Asm) {
        value(&mut a, 0);
        a.ret();
        assert!(a.here() < CODE + 0x8000);
        self.m.cpu.write(CODE, &a.bytes).unwrap();
        self.m.spawn("USER32", CODE, &[], 0x1_0000).unwrap();
    }

    fn finish(&mut self) {
        let result = self.m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }

    fn run(&mut self, a: Asm) {
        self.start(a);
        self.finish();
    }

    fn u32(&self, p: u32) -> u32 {
        self.m.cpu.read_u32(p).unwrap()
    }

    fn words<const N: usize>(&self, p: u32) -> [u32; N] {
        std::array::from_fn(|i| self.u32(p + 4 * i as u32))
    }

    fn log(&self) -> Vec<[u32; 4]> {
        let end = self.u32(LOG_NEXT);
        assert!(end <= LOG + 0x1_0000);
        (LOG..end).step_by(16).map(|p| self.words(p)).collect()
    }

    fn messages(&self) -> Vec<(u32, u32)> {
        self.log()
            .into_iter()
            .map(|[hwnd, msg, _, _]| (hwnd, msg))
            .collect()
    }

    fn retrieve(
        &mut self,
        a: &mut Asm,
        api: &str,
        hwnd: Arg,
        min: u32,
        max: u32,
        remove: bool,
    ) -> (u32, u32) {
        let msg = self.alloc(28);
        let mut args = vec![I(msg), hwnd, I(min), I(max)];
        if api == "PeekMessageA" {
            args.push(I(u32::from(remove)));
        }
        let result = self.call(a, api, &args);
        (msg, result)
    }
}

#[test]
fn queue_status_distinguishes_pending_changed_and_range_filtered_observation() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    let tid = t.call(&mut a, "kernel32.dll!GetCurrentThreadId", &[]);
    t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, false);
    t.call(
        &mut a,
        "PostThreadMessageA",
        &[M(tid), I(0x506), I(1), I(0)],
    );
    let first = t.call(&mut a, "GetQueueStatus", &[I(8)]);
    let again = t.call(&mut a, "GetQueueStatus", &[I(8)]);
    let allpost = t.call(&mut a, "GetQueueStatus", &[I(0x100)]);
    t.call(
        &mut a,
        "PostThreadMessageA",
        &[M(tid), I(0x513), I(2), I(0)],
    );
    let (_, miss) = t.retrieve(&mut a, "PeekMessageA", I(0), 0x599, 0x599, false);
    let filtered = t.call(&mut a, "GetQueueStatus", &[I(0x108)]);
    let other_category = t.call(&mut a, "GetQueueStatus", &[I(1)]);
    let (msg1, _) = t.retrieve(&mut a, "GetMessageA", I(0), 0, 0, true);
    let old = t.call(&mut a, "GetQueueStatus", &[I(0x108)]);
    let (msg2, _) = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, true);
    let empty = t.call(&mut a, "GetQueueStatus", &[I(0x108)]);
    t.run(a);
    assert_eq!(t.u32(first), 0x0008_0008);
    assert_eq!(t.u32(again), 0x0008_0000);
    assert_eq!(t.u32(allpost), 0x0100_0100);
    assert_eq!(t.u32(miss), 0);
    assert_eq!(t.u32(filtered), 0x0108_0100);
    assert_eq!(t.u32(other_category), 0);
    assert_eq!(t.words::<4>(msg1), [0, 0x506, 1, 0]);
    assert_eq!(t.u32(old), 0x0108_0000);
    assert_eq!(t.words::<4>(msg2), [0, 0x513, 2, 0]);
    assert_eq!(t.u32(empty), 0);
}

#[test]
fn generated_timer_expiry_becomes_new_again_after_removal() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    let timer = t.call(&mut a, "SetTimer", &[I(0), I(0), I(10), I(0)]);
    t.call(&mut a, "kernel32.dll!Sleep", &[I(10)]);
    let first = t.call(&mut a, "GetQueueStatus", &[I(0x10)]);
    let again = t.call(&mut a, "GetQueueStatus", &[I(0x10)]);
    let (msg, _) = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, true);
    // No queue query between removal and the next expiry.
    t.call(&mut a, "kernel32.dll!Sleep", &[I(10)]);
    let second = t.call(&mut a, "GetQueueStatus", &[I(0x10)]);
    t.call(&mut a, "KillTimer", &[I(0), M(timer)]);
    let empty = t.call(&mut a, "GetQueueStatus", &[I(0x10)]);
    t.run(a);
    assert_eq!(t.u32(first), 0x0010_0010);
    assert_eq!(t.u32(again), 0x0010_0000);
    assert_eq!(t.u32(msg + 4), wm::TIMER);
    assert_eq!(t.u32(second), 0x0010_0010);
    assert_eq!(t.u32(empty), 0);
}

#[test]
fn validating_then_invalidating_paint_becomes_new_without_an_intervening_query() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    let proc = t.wndproc(|_| {});
    let class = t.class(&mut a, proc);
    let hwnd = t.window(&mut a, class, WS_POPUP | WS_VISIBLE, I(0), 0);
    let (paint, _) = t.retrieve(&mut a, "PeekMessageA", M(hwnd), wm::PAINT, wm::PAINT, false);
    let old = t.call(&mut a, "GetQueueStatus", &[I(0x20)]);
    t.call(&mut a, "ValidateRect", &[M(hwnd), I(0)]);
    t.call(&mut a, "InvalidateRect", &[M(hwnd), I(0), I(0)]);
    let after_validate = t.call(&mut a, "GetQueueStatus", &[I(0x20)]);
    t.retrieve(&mut a, "PeekMessageA", M(hwnd), wm::PAINT, wm::PAINT, false);
    let ps = t.alloc(64);
    t.call(&mut a, "BeginPaint", &[M(hwnd), I(ps)]);
    t.call(&mut a, "EndPaint", &[M(hwnd), I(ps)]);
    t.call(&mut a, "InvalidateRect", &[M(hwnd), I(0), I(0)]);
    let after_begin_paint = t.call(&mut a, "GetQueueStatus", &[I(0x20)]);
    t.run(a);
    assert_eq!(t.u32(paint + 4), wm::PAINT);
    assert_eq!(t.u32(old), 0x0020_0000);
    assert_eq!(t.u32(after_validate), 0x0020_0020);
    assert_eq!(t.u32(after_begin_paint), 0x0020_0020);
}

#[test]
fn create_visible_windows_notifies_parent_then_shows_and_activates() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    let proc = t.wndproc(|_| {});
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP | WS_VISIBLE, I(0), 0);
    let child = t.window(&mut a, class, WS_CHILD | WS_VISIBLE, M(parent), 42);
    let other = t.window(&mut a, class, WS_POPUP | WS_VISIBLE, I(0), 0);
    t.run(a);
    let (parent, child, other) = (t.u32(parent), t.u32(child), t.u32(other));
    assert_ne!(parent, 0);
    assert_ne!(child, 0);
    assert_ne!(other, 0);
    assert_eq!(
        t.messages(),
        [
            (parent, wm::NCCREATE),
            (parent, wm::CREATE),
            (parent, wm::SIZE),
            (parent, wm::MOVE),
            (parent, wm::SHOWWINDOW),
            (parent, wm::ACTIVATEAPP),
            (parent, wm::ACTIVATE),
            (parent, wm::SETFOCUS),
            (child, wm::NCCREATE),
            (child, wm::CREATE),
            (child, wm::SIZE),
            (child, wm::MOVE),
            (parent, wm::PARENTNOTIFY),
            (child, wm::SHOWWINDOW),
            (other, wm::NCCREATE),
            (other, wm::CREATE),
            (other, wm::SIZE),
            (other, wm::MOVE),
            (other, wm::SHOWWINDOW),
            (parent, wm::ACTIVATE),
            (other, wm::ACTIVATE),
            (parent, wm::KILLFOCUS),
            (other, wm::SETFOCUS),
        ]
    );
    let log = t.log();
    assert_eq!(log[2], [parent, wm::SIZE, 0, 120 | (80 << 16)]);
    assert_eq!(log[3], [parent, wm::MOVE, 0, 20 | (30 << 16)]);
    assert_eq!(
        log[12],
        [parent, wm::PARENTNOTIFY, wm::CREATE | (42 << 16), child]
    );
    assert_eq!(log[19], [parent, wm::ACTIVATE, 0, other]);
    assert_eq!(log[20], [other, wm::ACTIVATE, 1, parent]);
}

#[test]
fn child_ex_noparentnotify_suppresses_creation_and_destruction_notification() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP, I(0), 0);
    a.mov_mem_imm(LOG_NEXT, LOG);
    let child = t.call(
        &mut a,
        "CreateWindowExA",
        &[
            I(WS_EX_NOPARENTNOTIFY),
            I(class),
            I(0),
            I(WS_CHILD),
            I(0),
            I(0),
            I(40),
            I(30),
            M(parent),
            I(42),
            I(CODE),
            I(0),
        ],
    );
    t.call(&mut a, "DestroyWindow", &[M(child)]);
    t.run(a);
    let child = t.u32(child);
    assert_ne!(child, 0);
    assert_eq!(
        t.messages(),
        [
            (child, wm::NCCREATE),
            (child, wm::CREATE),
            (child, wm::SIZE),
            (child, wm::MOVE),
            (child, wm::DESTROY),
            (child, wm::NCDESTROY)
        ]
    );
}

#[test]
fn rejected_creation_only_destroys_the_initialized_parts() {
    for (reject_message, reject_result, expected) in [
        (wm::NCCREATE, 0, vec![wm::NCCREATE, wm::NCDESTROY]),
        (
            wm::CREATE,
            u32::MAX,
            vec![wm::NCCREATE, wm::CREATE, wm::DESTROY, wm::NCDESTROY],
        ),
    ] {
        let mut t = T::new();
        let handle = t.alloc(4);
        let proc = t.wndproc(|a| {
            on_message(a, wm::NCCREATE, |a| {
                a.mov_eax_esp(4);
                store(a, handle);
            });
            on_message(a, reject_message, |a| {
                value(a, reject_result);
                a.ret_n(16);
            });
        });
        let mut a = Asm::new(CODE);
        let class = t.class(&mut a, proc);
        let result = t.window(&mut a, class, WS_POPUP | WS_VISIBLE, I(0), 0);
        let alive = t.call(&mut a, "IsWindow", &[M(handle)]);
        t.run(a);
        assert_eq!(t.u32(result), 0);
        assert_ne!(t.u32(handle), 0);
        assert_eq!(t.u32(alive), 0);
        assert_eq!(
            t.messages(),
            expected
                .into_iter()
                .map(|msg| (t.u32(handle), msg))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn destroy_owned_first_and_all_children_before_parent_ncdestroy() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    let proc = t.wndproc(|_| {});
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP, I(0), 0);
    let child = t.window(&mut a, class, WS_CHILD, M(parent), 1);
    let grandchild = t.window(&mut a, class, WS_CHILD, M(child), 2);
    let owned = t.window(&mut a, class, WS_POPUP, M(parent), 0);
    a.mov_mem_imm(LOG_NEXT, LOG);
    let destroyed = t.call(&mut a, "DestroyWindow", &[M(parent)]);
    let alive: Vec<_> = [parent, child, grandchild, owned]
        .map(|h| t.call(&mut a, "IsWindow", &[M(h)]))
        .into();
    t.run(a);
    let (p, c, g, o) = (t.u32(parent), t.u32(child), t.u32(grandchild), t.u32(owned));
    assert_eq!(t.u32(destroyed), 1);
    assert!(alive.into_iter().all(|p| t.u32(p) == 0));
    assert_eq!(
        t.messages(),
        [
            (o, wm::DESTROY),
            (o, wm::NCDESTROY),
            (p, wm::DESTROY),
            (c, wm::DESTROY),
            (g, wm::DESTROY),
            (g, wm::NCDESTROY),
            (c, wm::NCDESTROY),
            (p, wm::NCDESTROY)
        ]
    );
}

#[test]
fn parent_destroy_handler_can_destroy_its_child_once() {
    let mut t = T::new();
    let child_slot = t.alloc(4);
    let nested_result = t.alloc(4);
    let destroy = t.f("DestroyWindow");
    let parent_proc = t.wndproc(|a| {
        on_message(a, wm::DESTROY, |a| {
            invoke(a, destroy, &[M(child_slot)]);
            store(a, nested_result);
        });
    });
    let child_proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let pc = t.class(&mut a, parent_proc);
    let cc = t.class(&mut a, child_proc);
    let parent = t.window(&mut a, pc, WS_POPUP, I(0), 0);
    let child = t.window(&mut a, cc, WS_CHILD, M(parent), 1);
    a.mov_eax_mem(child);
    store(&mut a, child_slot);
    a.mov_mem_imm(LOG_NEXT, LOG);
    let result = t.call(&mut a, "DestroyWindow", &[M(parent)]);
    t.run(a);
    let (p, c) = (t.u32(parent), t.u32(child));
    assert_eq!(t.u32(result), 1);
    assert_eq!(t.u32(nested_result), 1);
    assert_eq!(
        t.messages(),
        [
            (p, wm::DESTROY),
            (p, wm::PARENTNOTIFY),
            (c, wm::DESTROY),
            (c, wm::NCDESTROY),
            (p, wm::NCDESTROY)
        ]
    );
}

#[test]
fn explicit_child_destroy_notifies_before_hide_and_preserves_child_id() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    let proc = t.wndproc(|_| {});
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP, I(0), 0);
    let child = t.window(&mut a, class, WS_CHILD | WS_VISIBLE, M(parent), 0x12345);
    a.mov_mem_imm(LOG_NEXT, LOG);
    let destroyed = t.call(&mut a, "DestroyWindow", &[M(child)]);
    let parent_alive = t.call(&mut a, "IsWindow", &[M(parent)]);
    t.run(a);
    let (p, c) = (t.u32(parent), t.u32(child));
    assert_eq!(t.u32(destroyed), 1);
    assert_eq!(t.u32(parent_alive), 1);
    assert_eq!(
        t.log(),
        [
            [p, wm::PARENTNOTIFY, wm::DESTROY | (0x2345 << 16), c],
            [c, wm::SHOWWINDOW, 0, 0],
            [c, wm::DESTROY, 0, 0],
            [c, wm::NCDESTROY, 0, 0],
        ]
    );
}

#[test]
fn nested_send_message_returns_through_every_guest_callback() {
    let mut t = T::new();
    let send = t.f("SendMessageA");
    let sleep = t.f("kernel32.dll!Sleep");
    let proc = t.wndproc(|a| {
        on_message(a, APP + 1, |a| {
            invoke(a, send, &[S(4), I(APP + 2), I(22), I(222)]);
            a.add_eax_imm(1).ret_n(16);
        });
        on_message(a, APP + 2, |a| {
            invoke(a, send, &[S(4), I(APP + 3), I(33), I(333)]);
            a.add_eax_imm(10).ret_n(16);
        });
        on_message(a, APP + 3, |a| {
            invoke(a, sleep, &[I(5)]);
            value(a, 100);
            a.ret_n(16);
        });
    });
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let hwnd = t.window(&mut a, class, WS_POPUP, I(0), 0);
    a.mov_mem_imm(LOG_NEXT, LOG);
    let result = t.call(
        &mut a,
        "SendMessageA",
        &[M(hwnd), I(APP + 1), I(11), I(111)],
    );
    let before = t.m.sys.clock.now_us();
    t.run(a);
    let hwnd = t.u32(hwnd);
    assert_eq!(t.u32(result), 111);
    assert_eq!(
        t.log(),
        [
            [hwnd, APP + 1, 11, 111],
            [hwnd, APP + 2, 22, 222],
            [hwnd, APP + 3, 33, 333]
        ]
    );
    assert_eq!(t.m.sys.clock.now_us(), before + 5_000);
}

#[test]
fn posted_messages_filter_descendants_ranges_and_thread_messages() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP, I(0), 0);
    let child = t.window(&mut a, class, WS_CHILD, M(parent), 1);
    let other = t.window(&mut a, class, WS_POPUP, I(0), 0);
    t.call(&mut a, "PostMessageA", &[M(other), I(APP + 1), I(1), I(10)]);
    t.call(&mut a, "PostMessageA", &[M(child), I(APP + 2), I(2), I(20)]);
    t.call(&mut a, "PostMessageA", &[I(0), I(APP + 3), I(3), I(30)]);
    let thread = t.retrieve(&mut a, "PeekMessageA", I(u32::MAX), 0, 0, true);
    let peek1 = t.retrieve(&mut a, "PeekMessageA", M(parent), APP + 2, APP + 2, false);
    let peek2 = t.retrieve(&mut a, "PeekMessageA", M(parent), 0, 0, false);
    let get = t.retrieve(&mut a, "GetMessageA", M(parent), APP + 2, APP + 2, true);
    let filtered = t.retrieve(&mut a, "PeekMessageA", I(0), APP + 2, APP + 3, true);
    let remaining = t.retrieve(&mut a, "GetMessageA", I(0), 0, 0, true);
    t.run(a);
    assert_eq!(t.words::<4>(thread.0), [0, APP + 3, 3, 30]);
    for (msg, result) in [peek1, peek2, get] {
        assert_eq!(t.u32(result), 1);
        assert_eq!(t.words::<4>(msg), [t.u32(child), APP + 2, 2, 20]);
    }
    assert_eq!(t.u32(filtered.1), 0);
    assert_eq!(t.words::<4>(remaining.0), [t.u32(other), APP + 1, 1, 10]);
}

#[test]
fn retrieval_prioritizes_posted_then_quit_then_paint_then_timer() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let hwnd = t.window(&mut a, class, WS_POPUP | WS_VISIBLE, I(0), 0);
    t.call(&mut a, "SetTimer", &[M(hwnd), I(42), I(10), I(0)]);
    t.call(&mut a, "kernel32.dll!Sleep", &[I(20)]);
    t.call(&mut a, "PostQuitMessage", &[I(7)]);
    t.call(&mut a, "PostMessageA", &[M(hwnd), I(APP), I(5), I(6)]);
    let posted = t.retrieve(&mut a, "GetMessageA", I(0), 0, 0, true);
    let quit_peek = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, false);
    let quit = t.retrieve(&mut a, "GetMessageA", I(0), 0, 0, true);
    let paint = t.retrieve(&mut a, "GetMessageA", I(0), 0, 0, true);
    let paint_again = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, true);
    t.call(&mut a, "DispatchMessageA", &[I(paint.0)]);
    let timer = t.retrieve(&mut a, "GetMessageA", I(0), 0, 0, true);
    t.call(&mut a, "KillTimer", &[M(hwnd), I(42)]);
    let empty = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, true);
    t.run(a);
    assert_eq!(t.words::<4>(posted.0), [t.u32(hwnd), APP, 5, 6]);
    assert_eq!(t.u32(posted.1), 1);
    for (msg, result) in [quit_peek, quit] {
        assert_eq!(t.words::<4>(msg), [0, wm::QUIT, 7, 0]);
        assert_eq!(t.u32(result), u32::from(msg == quit_peek.0));
    }
    for (msg, _) in [paint, paint_again] {
        assert_eq!(t.words::<4>(msg), [t.u32(hwnd), wm::PAINT, 0, 0]);
    }
    assert_eq!(t.words::<4>(timer.0), [t.u32(hwnd), wm::TIMER, 42, 0]);
    assert_eq!(t.u32(empty.1), 0);
}

#[test]
fn quit_bypasses_window_and_message_range_filters() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let hwnd = t.window(&mut a, class, WS_POPUP, I(0), 0);
    t.call(&mut a, "PostQuitMessage", &[I(19)]);
    let peek = t.retrieve(&mut a, "PeekMessageA", M(hwnd), APP, APP, false);
    let get = t.retrieve(&mut a, "GetMessageA", M(hwnd), APP + 1, APP + 1, true);
    let empty = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, true);
    t.run(a);
    assert_eq!(t.u32(peek.1), 1);
    assert_eq!(t.u32(get.1), 0);
    for (msg, _) in [peek, get] {
        assert_eq!(t.words::<4>(msg), [0, wm::QUIT, 19, 0]);
    }
    assert_eq!(t.u32(empty.1), 0);
}

#[test]
fn empty_get_message_blocks_until_the_virtual_timer_deadline() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    let timer = t.call(&mut a, "SetTimer", &[I(0), I(0), I(1), I(0)]);
    let (msg, result) = t.retrieve(&mut a, "GetMessageA", I(0), 0, 0, true);
    let marker = t.alloc(4);
    a.mov_mem_imm(marker, 0x1234);
    let before = t.m.sys.clock.now_us();
    t.start(a);
    assert!(
        matches!(t.m.run(), RunResult::Idle { next_deadline: Some(t) } if t == before + 10_000)
    );
    assert_eq!(t.u32(marker), 0);
    assert_eq!(t.m.sys.clock.now_us(), before);
    t.finish();
    assert_eq!(t.u32(marker), 0x1234);
    assert_eq!(t.u32(result), 1);
    assert_eq!(t.words::<4>(msg), [0, wm::TIMER, t.u32(timer), 0]);
    assert_eq!(t.m.sys.clock.now_us(), before + 10_000);
}

#[test]
fn filtered_get_message_sleeps_with_unmatched_posted_messages() {
    let mut t = T::new();
    let mut a = Asm::new(CODE);
    t.call(&mut a, "PostMessageA", &[I(0), I(APP), I(77), I(88)]);
    let timer = t.call(&mut a, "SetTimer", &[I(0), I(0), I(25), I(0)]);
    let (msg, result) = t.retrieve(&mut a, "GetMessageA", I(0), wm::TIMER, wm::TIMER, true);
    let remaining = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, true);
    let before = t.m.sys.clock.now_us();
    t.start(a);
    assert!(
        matches!(t.m.run(), RunResult::Idle { next_deadline: Some(at) } if at == before + 25_000)
    );
    assert_eq!(t.m.sys.clock.now_us(), before);
    t.finish();
    assert_eq!(t.u32(result), 1);
    assert_eq!(t.words::<4>(msg), [0, wm::TIMER, t.u32(timer), 0]);
    assert_eq!(t.u32(remaining.1), 1);
    assert_eq!(t.words::<4>(remaining.0), [0, APP, 77, 88]);
    assert_eq!(t.m.sys.clock.now_us(), before + 25_000);
}

#[test]
fn cross_thread_window_timer_wakes_and_runs_on_the_window_owner() {
    let mut t = T::new();
    let callback_thread = t.alloc(4);
    let thread_id = t.f("kernel32.dll!GetCurrentThreadId");
    let proc = t.wndproc(|a| {
        on_message(a, wm::TIMER, |a| {
            invoke(a, thread_id, &[]);
            store(a, callback_thread);
        });
    });
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let hwnd = t.window(&mut a, class, WS_POPUP, I(0), 0);
    let owner_thread = t.call(&mut a, "kernel32.dll!GetCurrentThreadId", &[]);
    let (msg, got) = t.retrieve(&mut a, "GetMessageA", M(hwnd), wm::TIMER, wm::TIMER, true);
    t.call(&mut a, "DispatchMessageA", &[I(msg)]);
    let killed = t.call(&mut a, "KillTimer", &[M(hwnd), I(10000)]);
    let before = t.m.sys.clock.now_us();
    t.start(a);

    let worker = CODE + 0x7000;
    let mut a = Asm::new(worker);
    // The owner is already blocked without a deadline when this timer is installed.
    invoke(&mut a, t.f("kernel32.dll!Sleep"), &[I(10)]);
    let set = t.call(&mut a, "SetTimer", &[M(hwnd), I(10000), I(20), I(0)]);
    let worker_timer = t.retrieve(&mut a, "PeekMessageA", I(0), wm::TIMER, wm::TIMER, true);
    value(&mut a, 0);
    a.ret();
    t.m.cpu.write(worker, &a.bytes).unwrap();
    t.m.spawn("timer-setter", worker, &[], 0x1_0000).unwrap();
    t.finish();

    assert_eq!(t.u32(set), 10000);
    assert_eq!(t.u32(got), 1);
    assert_eq!(t.words::<4>(msg), [t.u32(hwnd), wm::TIMER, 10000, 0]);
    assert_eq!(t.u32(callback_thread), t.u32(owner_thread));
    assert_eq!(t.u32(killed), 1);
    assert_eq!(t.u32(worker_timer.1), 0);
    assert!(
        t.m.ext
            .get::<Win32>()
            .user
            .queues
            .values()
            .all(|q| q.timers.is_empty())
    );
    assert_eq!(t.m.sys.clock.now_us(), before + 30_000);
}

#[test]
fn filtered_get_message_does_not_wake_for_excluded_timers() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let hwnd = t.window(&mut a, class, WS_POPUP, I(0), 0);
    t.call(&mut a, "SetTimer", &[M(hwnd), I(42), I(10), I(0)]);
    let (msg, result) = t.retrieve(&mut a, "GetMessageA", M(hwnd), APP, APP, true);
    let before = t.m.sys.clock.now_us();
    t.start(a);
    let worker = CODE + 0x7000;
    let mut a = Asm::new(worker);
    invoke(&mut a, t.f("kernel32.dll!Sleep"), &[I(30)]);
    invoke(&mut a, t.f("PostMessageA"), &[M(hwnd), I(APP), I(1), I(2)]);
    value(&mut a, 0);
    a.ret();
    t.m.cpu.write(worker, &a.bytes).unwrap();
    t.m.spawn("message-poster", worker, &[], 0x1_0000).unwrap();
    assert!(
        matches!(t.m.run(), RunResult::Idle { next_deadline: Some(at) } if at == before + 30_000)
    );
    t.finish();
    assert_eq!(t.u32(result), 1);
    assert_eq!(t.words::<4>(msg), [t.u32(hwnd), APP, 1, 2]);
    assert_eq!(t.m.sys.clock.now_us(), before + 30_000);
}

#[test]
fn timer_filters_include_descendants_and_select_only_thread_timers() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP, I(0), 0);
    let child = t.window(&mut a, class, WS_CHILD, M(parent), 1);
    t.call(&mut a, "SetTimer", &[M(child), I(42), I(10), I(0)]);
    let timer = t.call(&mut a, "SetTimer", &[I(0), I(0), I(25), I(0)]);
    let descendant = t.retrieve(&mut a, "GetMessageA", M(parent), wm::TIMER, wm::TIMER, true);
    let thread = t.retrieve(
        &mut a,
        "GetMessageA",
        I(u32::MAX),
        wm::TIMER,
        wm::TIMER,
        true,
    );
    let before = t.m.sys.clock.now_us();
    t.run(a);
    assert_eq!(t.u32(descendant.1), 1);
    assert_eq!(t.words::<4>(descendant.0), [t.u32(child), wm::TIMER, 42, 0]);
    assert_eq!(t.u32(thread.1), 1);
    assert_eq!(t.words::<4>(thread.0), [0, wm::TIMER, t.u32(timer), 0]);
    assert_eq!(t.m.sys.clock.now_us(), before + 25_000);
}

#[test]
fn button_mouse_clicks_notify_parent_toggle_checkbox_and_respect_radio_groups() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP, I(0), 0);
    let button = t.text("BUTTON");
    let checkbox = t.window(&mut a, button, WS_CHILD | 3, M(parent), 10);
    let radios = [WS_GROUP, 0, WS_GROUP, 0]
        .map(|group| t.window(&mut a, button, WS_CHILD | group | 9, M(parent), 20));
    a.mov_mem_imm(LOG_NEXT, LOG);
    let mut checks = Vec::new();
    for hwnd in [checkbox, checkbox, radios[0], radios[2], radios[1]] {
        t.call(
            &mut a,
            "SendMessageA",
            &[M(hwnd), I(wm::LBUTTONDOWN), I(1), I(0)],
        );
        t.call(
            &mut a,
            "SendMessageA",
            &[M(hwnd), I(wm::LBUTTONUP), I(0), I(0)],
        );
        checks.push(t.call(
            &mut a,
            "SendMessageA",
            &[M(hwnd), I(BM_GETCHECK), I(0), I(0)],
        ));
    }
    let radio_checks = radios.map(|hwnd| {
        t.call(
            &mut a,
            "SendMessageA",
            &[M(hwnd), I(BM_GETCHECK), I(0), I(0)],
        )
    });
    t.run(a);
    assert_eq!(
        checks.into_iter().map(|p| t.u32(p)).collect::<Vec<_>>(),
        [1, 0, 1, 1, 1]
    );
    assert_eq!(radio_checks.map(|p| t.u32(p)), [0, 1, 1, 0]);
    let parent = t.u32(parent);
    assert_eq!(
        t.log(),
        [
            [parent, wm::COMMAND, 10, t.u32(checkbox)],
            [parent, wm::COMMAND, 10, t.u32(checkbox)],
            [parent, wm::COMMAND, 20, t.u32(radios[0])],
            [parent, wm::COMMAND, 20, t.u32(radios[2])],
            [parent, wm::COMMAND, 20, t.u32(radios[1])]
        ]
    );
}

#[test]
fn bm_click_completes_checkbox_and_parent_notification_synchronously() {
    let mut t = T::new();
    let notified = t.alloc(4);
    let sleep = t.f("kernel32.dll!Sleep");
    let proc = t.wndproc(|a| {
        on_message(a, wm::COMMAND, |a| {
            invoke(a, sleep, &[I(5)]);
            a.mov_mem_imm(notified, 1);
        });
    });
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let parent = t.window(&mut a, class, WS_POPUP, I(0), 0);
    let button = t.text("BUTTON");
    let checkbox = t.window(&mut a, button, WS_CHILD | 3, M(parent), 42);
    a.mov_mem_imm(LOG_NEXT, LOG);
    t.call(&mut a, "SendMessageA", &[M(checkbox), I(0xf5), I(0), I(0)]); // BM_CLICK
    let check = t.call(
        &mut a,
        "SendMessageA",
        &[M(checkbox), I(BM_GETCHECK), I(0), I(0)],
    );
    let completed_ms = t.call(&mut a, "kernel32.dll!GetTickCount", &[]);
    let empty = t.retrieve(&mut a, "PeekMessageA", I(0), 0, 0, true);
    let before = t.m.sys.clock.now_us();
    t.run(a);
    assert_eq!(t.u32(check), 1);
    assert_eq!(t.u32(notified), 1);
    assert_eq!(t.u32(empty.1), 0);
    assert_eq!(t.log(), [[t.u32(parent), wm::COMMAND, 42, t.u32(checkbox)]]);
    assert_eq!(u64::from(t.u32(completed_ms)), before / 1_000 + 5);
}

#[test]
fn dialog_templates_preserve_creation_data_and_extended_control_ids() {
    for extended in [false, true] {
        let mut t = T::new();
        let data = t.alloc(4);
        let control = t.wndproc(|a| {
            on_message(a, wm::CREATE, |a| {
                a.mov_eax_esp(16);
                a.bytes.extend_from_slice(&[0x8b, 0x00]); // mov eax, [eax] (lpCreateParams)
                store(a, data);
            });
        });
        let proc = t.wndproc(|a| {
            value(a, 0);
            a.ret_n(16);
        });
        let mut a = Asm::new(CODE);
        t.class(&mut a, control);
        let template = t.bytes(&dialog_template(
            extended,
            Some(&format!("User32Test{control:x}")),
        ));
        let dlg = t.call(
            &mut a,
            "CreateDialogIndirectParamA",
            &[I(CODE), I(template), I(0), I(proc), I(0x1234)],
        );
        let base = if extended { 0x1_0000 } else { 0 };
        let first = t.call(&mut a, "GetDlgItem", &[M(dlg), I(base + 41)]);
        let second = t.call(&mut a, "GetDlgItem", &[M(dlg), I(base + 42)]);
        let id = t.call(&mut a, "GetDlgCtrlID", &[M(second)]);
        let font = t.call(
            &mut a,
            "SendMessageA",
            &[M(first), I(wm::GETFONT), I(0), I(0)],
        );
        t.call(&mut a, "DestroyWindow", &[M(dlg)]);
        let alive = t.call(&mut a, "IsWindow", &[M(first)]);
        t.run(a);
        assert_ne!(t.u32(dlg), 0);
        assert_ne!(t.u32(first), 0);
        assert_ne!(t.u32(second), 0);
        assert_eq!(t.u32(id), base + 42);
        assert_ne!(t.u32(font), 0);
        assert_eq!(t.u32(alive), 0);
        let p = t.u32(data);
        assert_ne!(p, 0);
        assert_eq!(t.m.cpu.read_u16(p).unwrap(), 4);
        assert_eq!(t.m.cpu.read_u16(p + 2).unwrap(), 0xabcd);
        assert_eq!(t.m.cpu.read_u16(p + 4).unwrap(), 0xef01);
        assert!(
            t.log()
                .contains(&[t.u32(dlg), wm::INITDIALOG, t.u32(first), 0x1234])
        );
    }
}

#[test]
fn modal_dialog_waits_for_timer_and_restores_its_owner_after_end_dialog() {
    for extended in [false, true] {
        let mut t = T::new();
        let owner_proc = t.wndproc(|_| {});
        let mut a = Asm::new(CODE);
        let class = t.class(&mut a, owner_proc);
        let owner = t.window(&mut a, class, WS_POPUP, I(0), 0);
        let dlg = t.alloc(4);
        let enabled_inside = t.alloc(4);
        let end_result = t.alloc(4);
        let alive_inside = t.alloc(4);
        let set_timer = t.f("SetTimer");
        let end = t.f("EndDialog");
        let enabled = t.f("IsWindowEnabled");
        let alive = t.f("IsWindow");
        let proc = t.wndproc(|a| {
            on_message(a, wm::INITDIALOG, |a| {
                a.mov_eax_esp(4);
                store(a, dlg);
                invoke(a, enabled, &[M(owner)]);
                store(a, enabled_inside);
                invoke(a, set_timer, &[S(4), I(7), I(10), I(0)]);
            });
            on_message(a, wm::TIMER, |a| {
                invoke(a, end, &[S(4), I(0xcafe)]);
                store(a, end_result);
                invoke(a, alive, &[S(4)]);
                store(a, alive_inside);
                value(a, 1);
                a.ret_n(16);
            });
            value(a, 0);
            a.ret_n(16);
        });
        let template = t.bytes(&dialog_template(extended, None));
        let result = t.call(
            &mut a,
            "DialogBoxIndirectParamA",
            &[I(CODE), I(template), M(owner), I(proc), I(123)],
        );
        let enabled_after = t.call(&mut a, "IsWindowEnabled", &[M(owner)]);
        let alive_after = t.call(&mut a, "IsWindow", &[M(dlg)]);
        let marker = t.alloc(4);
        a.mov_mem_imm(marker, 1);
        let before = t.m.sys.clock.now_us();
        t.start(a);
        assert!(
            matches!(t.m.run(), RunResult::Idle { next_deadline: Some(at) } if at == before + 10_000)
        );
        assert_eq!(t.u32(marker), 0);
        assert_eq!(t.u32(enabled_inside), 0);
        t.finish();
        assert_eq!(t.u32(result), 0xcafe);
        assert_eq!(t.u32(end_result), 1);
        assert_eq!(t.u32(alive_inside), 1); // EndDialog defers destruction until the callback returns.
        assert_eq!(t.u32(alive_after), 0);
        assert_eq!(t.u32(enabled_after), 1);
        assert_eq!(t.u32(marker), 1);
        assert!(t.log().contains(&[t.u32(dlg), wm::TIMER, 7, 0]));
        assert_eq!(t.m.sys.clock.now_us(), before + 10_000);
    }
}

#[test]
fn end_dialog_during_initialization_returns_without_showing_or_activating() {
    for extended in [false, true] {
        let mut t = T::new();
        let dlg = t.alloc(4);
        let end = t.f("EndDialog");
        let proc = t.wndproc(|a| {
            on_message(a, wm::INITDIALOG, |a| {
                a.mov_eax_esp(4);
                store(a, dlg);
                invoke(a, end, &[S(4), S(16)]);
                value(a, 1); // Even TRUE must not set focus after EndDialog.
                a.ret_n(16);
            });
            value(a, 0);
            a.ret_n(16);
        });
        let template = t.bytes(&dialog_template(extended, None));
        let mut a = Asm::new(CODE);
        let result = t.call(
            &mut a,
            "DialogBoxIndirectParamA",
            &[I(CODE), I(template), I(0), I(proc), I(0x77)],
        );
        let alive = t.call(&mut a, "IsWindow", &[M(dlg)]);
        t.run(a);
        assert_eq!(t.u32(result), 0x77);
        assert_eq!(t.u32(alive), 0);
        assert!(
            !t.log()
                .iter()
                .any(|m| matches!(m[1], wm::SHOWWINDOW | wm::ACTIVATE | wm::SETFOCUS))
        );
    }
}

#[test]
fn auto_dialog_clicks_the_default_button_after_its_virtual_deadline() {
    for extended in [false, true] {
        let mut t = T::with_auto_dialog(Some(25));
        let dlg = t.alloc(4);
        let button = t.alloc(4);
        let end = t.f("EndDialog");
        let item = t.f("GetDlgItem");
        let proc = t.wndproc(|a| {
            on_message(a, wm::INITDIALOG, |a| {
                a.mov_eax_esp(4);
                store(a, dlg);
                invoke(a, item, &[S(4), I(if extended { 0x1_002a } else { 42 })]);
                store(a, button);
            });
            on_message(a, wm::COMMAND, |a| {
                invoke(a, end, &[S(4), I(0xbeef)]);
                value(a, 1);
                a.ret_n(16);
            });
            value(a, 0);
            a.ret_n(16);
        });
        let template = t.bytes(&dialog_template(extended, None));
        let mut a = Asm::new(CODE);
        let result = t.call(
            &mut a,
            "DialogBoxIndirectParamA",
            &[I(CODE), I(template), I(0), I(proc), I(0)],
        );
        let before = t.m.sys.clock.now_us();
        t.start(a);
        assert!(
            matches!(t.m.run(), RunResult::Idle { next_deadline: Some(at) } if at == before + 25_000)
        );
        assert!(!t.log().iter().any(|m| m[1] == wm::COMMAND));
        t.finish();
        assert_eq!(t.u32(result), 0xbeef);
        let commands: Vec<_> = t
            .log()
            .into_iter()
            .filter(|m| m[1] == wm::COMMAND)
            .collect();
        assert_eq!(commands, [[t.u32(dlg), wm::COMMAND, 42, t.u32(button)]]);
        assert_eq!(t.m.sys.clock.now_us(), before + 25_000);
    }
}

#[test]
fn adjust_client_rect_and_window_moves_use_consistent_geometry() {
    let mut t = T::new();
    let proc = t.wndproc(|_| {});
    let mut a = Asm::new(CODE);
    let class = t.class(&mut a, proc);
    let adjusted = t.bytes(
        &[0_u32, 0, 200, 100]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let style = WS_POPUP | WS_CAPTION | WS_THICKFRAME;
    t.call(
        &mut a,
        "AdjustWindowRectEx",
        &[I(adjusted), I(style), I(0), I(WS_EX_CLIENTEDGE)],
    );
    let hwnd = t.call(
        &mut a,
        "CreateWindowExA",
        &[
            I(WS_EX_CLIENTEDGE),
            I(class),
            I(0),
            I(style),
            I(20),
            I(30),
            I(220),
            I(143),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    );
    let client = t.alloc(16);
    t.call(&mut a, "GetClientRect", &[M(hwnd), I(client)]);
    a.mov_mem_imm(LOG_NEXT, LOG);
    let flags = SWP_NOZORDER | SWP_NOACTIVATE;
    t.call(
        &mut a,
        "SetWindowPos",
        &[M(hwnd), I(0), I(40), I(50), I(240), I(163), I(flags)],
    );
    let moved = t.alloc(16);
    t.call(&mut a, "GetWindowRect", &[M(hwnd), I(moved)]);
    t.call(
        &mut a,
        "SetWindowPos",
        &[
            M(hwnd),
            I(0),
            I(900),
            I(900),
            I(260),
            I(183),
            I(flags | SWP_NOMOVE),
        ],
    );
    let resized = t.alloc(16);
    t.call(&mut a, "GetWindowRect", &[M(hwnd), I(resized)]);
    t.call(
        &mut a,
        "SetWindowPos",
        &[
            M(hwnd),
            I(0),
            I(60),
            I(70),
            I(999),
            I(999),
            I(flags | SWP_NOSIZE),
        ],
    );
    let repositioned = t.alloc(16);
    t.call(&mut a, "GetWindowRect", &[M(hwnd), I(repositioned)]);
    t.call(
        &mut a,
        "MoveWindow",
        &[M(hwnd), I(80), I(90), I(280), I(203), I(0)],
    );
    let final_rect = t.alloc(16);
    let final_client = t.alloc(16);
    t.call(&mut a, "GetWindowRect", &[M(hwnd), I(final_rect)]);
    t.call(&mut a, "GetClientRect", &[M(hwnd), I(final_client)]);
    t.run(a);
    assert_eq!(
        t.words::<4>(adjusted).map(|v| v as i32),
        [-10, -33, 210, 110]
    );
    assert_eq!(t.words::<4>(client), [0, 0, 200, 100]);
    assert_eq!(t.words::<4>(moved), [40, 50, 280, 213]);
    assert_eq!(t.words::<4>(resized), [40, 50, 300, 233]);
    assert_eq!(t.words::<4>(repositioned), [60, 70, 320, 253]);
    assert_eq!(t.words::<4>(final_rect), [80, 90, 360, 293]);
    assert_eq!(t.words::<4>(final_client), [0, 0, 260, 160]);
    let geometry: Vec<_> = t
        .log()
        .into_iter()
        .filter(|m| matches!(m[1], wm::MOVE | wm::SIZE))
        .collect();
    let hwnd = t.u32(hwnd);
    assert_eq!(
        geometry,
        [
            [hwnd, wm::MOVE, 0, 50 | (83 << 16)],
            [hwnd, wm::SIZE, 0, 220 | (120 << 16)],
            [hwnd, wm::SIZE, 0, 240 | (140 << 16)],
            [hwnd, wm::MOVE, 0, 70 | (103 << 16)],
            [hwnd, wm::MOVE, 0, 90 | (123 << 16)],
            [hwnd, wm::SIZE, 0, 260 | (160 << 16)],
        ]
    );
}
