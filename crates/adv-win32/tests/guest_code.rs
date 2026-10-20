//! APIs that run as guest code instead of host traps. Every backend runs the same
//! call sequence often enough for the JIT to compile it, with guest code on and
//! off; on Windows the host's own user32 is the reference.

use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
#[cfg(not(target_arch = "aarch64"))]
use adv_machine::layout::{GUEST_CODE_BASE, GUEST_CODE_SIZE};
use adv_machine::{Clock, Machine, RunResult};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const RECTS: u32 = DATA + 0x1000;
const RESULTS: u32 = DATA + 0x4000;
const COUNT: u32 = DATA;
const SAVED: u32 = DATA + 0x10;
const NULL: usize = usize::MAX;
const ITERATIONS: u32 = 300;
const SENTINELS: [u32; 4] = [0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444];

const SAMPLES: [[i32; 4]; 10] = [
    [0, 0, 10, 10],
    [5, 5, 15, 15],
    [20, 20, 30, 30],
    [0, 0, 0, 10],
    [10, 10, 0, 0],
    [-5, -5, 3, 3],
    [i32::MIN, i32::MIN, i32::MAX, i32::MAX],
    [3, 3, 3, 3],
    [10, 0, 20, 10],
    [-7, 4, -2, 9],
];

#[derive(Clone, Copy, Debug)]
enum Op {
    SetRectEmpty(usize),
    IsRectEmpty(usize),
    IntersectRect(usize, usize, usize),
    UnionRect(usize, usize, usize),
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::SetRectEmpty(..) => "SetRectEmpty",
            Op::IsRectEmpty(..) => "IsRectEmpty",
            Op::IntersectRect(..) => "IntersectRect",
            Op::UnionRect(..) => "UnionRect",
        }
    }

    fn args(self) -> Vec<usize> {
        match self {
            Op::SetRectEmpty(a) | Op::IsRectEmpty(a) => vec![a],
            Op::IntersectRect(a, b, c) | Op::UnionRect(a, b, c) => vec![a, b, c],
        }
    }
}

/// Fresh destinations for every pair, plus calls whose destination is a source,
/// so the inputs drift across iterations exactly as they do on Windows.
fn plan() -> (Vec<[i32; 4]>, Vec<Op>) {
    let mut rects = SAMPLES.to_vec();
    let mut ops = Vec::new();
    let n = SAMPLES.len();
    for i in 0..n {
        ops.push(Op::IsRectEmpty(i));
        for j in 0..n {
            for intersect in [true, false] {
                let dst = rects.len();
                rects.push([7, 7, 7, 7]);
                ops.push(if intersect {
                    Op::IntersectRect(dst, i, j)
                } else {
                    Op::UnionRect(dst, i, j)
                });
            }
        }
    }
    let alias = rects.len();
    rects.extend_from_slice(&[[0, 0, 10, 10], [2, -3, 8, 4], [-1, 1, 5, 12]]);
    ops.extend([
        Op::IntersectRect(alias, alias, alias + 1),
        Op::UnionRect(alias + 2, alias + 2, alias),
        Op::UnionRect(alias + 1, alias, alias + 1),
        Op::IsRectEmpty(alias),
        Op::IsRectEmpty(NULL),
        Op::SetRectEmpty(NULL),
        Op::IntersectRect(NULL, 0, 1),
        Op::IntersectRect(alias, NULL, 1),
        Op::IntersectRect(alias, 1, NULL),
        Op::UnionRect(NULL, 0, 1),
        Op::UnionRect(alias, NULL, 1),
        Op::UnionRect(alias, 1, NULL),
    ]);
    let scratch = rects.len();
    rects.push([1, 2, 3, 4]);
    ops.push(Op::SetRectEmpty(scratch));
    (rects, ops)
}

struct Outcome {
    rects: Vec<[i32; 4]>,
    results: Vec<u32>,
}

fn machine<B: Backend>(cpu: B, guest_code: bool) -> Machine<B> {
    machine_with(cpu, guest_code, Clock::virtual_time())
}

fn machine_with<B: Backend>(cpu: B, guest_code: bool, clock: Clock) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut m = Machine::new(cpu, registry, clock).unwrap();
    m.sys.thunks.set_guest_code(&mut m.cpu, guest_code).unwrap();
    m.cpu.map(CODE, 0x1_0000, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x1_0000, Prot::RW).unwrap();
    let root = std::env::temp_dir().join("pal-guest-code-no-files");
    let cfg = Win32Config::new(root.join("game"), "guest-code-test.exe", root.join("data"));
    Win32::install(&mut m, cfg).unwrap();
    m
}

fn raw(a: &mut Asm, op: &[u8], value: u32) {
    a.bytes.extend_from_slice(op);
    a.bytes.extend_from_slice(&value.to_le_bytes());
}

/// Runs `body` `ITERATIONS` times with sentinel callee-saved registers and
/// returns them with ESP's change.
fn run_loop<B: Backend>(m: &mut Machine<B>, body: impl Fn(&mut Asm)) -> [u32; 5] {
    let mut a = Asm::new(CODE);
    a.bytes.extend_from_slice(&[0x53, 0x56, 0x57, 0x55]); // push ebx, esi, edi, ebp
    for (op, value) in [0xbb, 0xbe, 0xbf, 0xbd].into_iter().zip(SENTINELS) {
        raw(&mut a, &[op], value);
    }
    raw(&mut a, &[0x89, 0x25], SAVED + 16); // mov [SAVED+16], esp
    raw(&mut a, &[0xc7, 0x05], COUNT); // mov dword [COUNT], ITERATIONS
    a.bytes.extend_from_slice(&ITERATIONS.to_le_bytes());
    let top = a.here();
    body(&mut a);
    raw(&mut a, &[0xff, 0x0d], COUNT); // dec dword [COUNT]
    let next = a.here() + 6;
    raw(&mut a, &[0x0f, 0x85], top.wrapping_sub(next)); // jnz top
    for (i, op) in [0x1d, 0x35, 0x3d, 0x2d].into_iter().enumerate() {
        raw(&mut a, &[0x89, op], SAVED + 4 * i as u32); // mov [SAVED+i], reg
    }
    a.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    raw(&mut a, &[0x2b, 0x05], SAVED + 16); // sub eax, [SAVED+16]
    raw(&mut a, &[0xa3], SAVED + 16); // mov [SAVED+16], eax
    a.bytes.extend_from_slice(&[0x5d, 0x5f, 0x5e, 0x5b]);
    raw(&mut a, &[0xb8], 0); // mov eax, 0
    a.ret();
    assert!(a.here() < CODE + 0x1_0000);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("GUEST-CODE", CODE, &[], 0x1_0000).unwrap();
    let result = m.run_virtual(10_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    std::array::from_fn(|i| m.cpu.read_u32(SAVED + 4 * i as u32).unwrap())
}

fn emulate<B: Backend>(cpu: B, guest_code: bool) -> (Outcome, Machine<B>) {
    let (rects, ops) = plan();
    let mut m = machine(cpu, guest_code);
    for (i, r) in rects.iter().enumerate() {
        for (k, v) in r.iter().enumerate() {
            m.cpu
                .write_u32(RECTS + 16 * i as u32 + 4 * k as u32, *v as u32)
                .unwrap();
        }
    }
    let targets: Vec<u32> = ops
        .iter()
        .map(|op| m.resolve_import("user32.dll", op.name()).unwrap())
        .collect();
    let saved = run_loop(&mut m, |a| {
        for (i, (op, &target)) in ops.iter().zip(&targets).enumerate() {
            for arg in op.args().into_iter().rev() {
                let p = if arg == NULL {
                    0
                } else {
                    RECTS + 16 * arg as u32
                };
                a.push_imm(p);
            }
            a.call(target);
            raw(a, &[0xa3], RESULTS + 4 * i as u32); // mov [RESULTS+i], eax
        }
    });
    assert_eq!(saved[..4], SENTINELS, "callee-saved registers");
    assert_eq!(saved[4], 0, "stack balance");
    let outcome = Outcome {
        rects: (0..rects.len() as u32)
            .map(|i| {
                std::array::from_fn(|k| {
                    m.cpu.read_u32(RECTS + 16 * i + 4 * k as u32).unwrap() as i32
                })
            })
            .collect(),
        results: (0..ops.len() as u32)
            .map(|i| m.cpu.read_u32(RESULTS + 4 * i).unwrap())
            .collect(),
    };
    (outcome, m)
}

fn jit() -> JitCpu {
    let mut cpu = JitCpu::new(PARK_ADDR, true).unwrap();
    cpu.set_async_compilation(false);
    #[cfg(not(target_arch = "aarch64"))]
    cpu.set_profiling(true);
    cpu
}

/// a64 keeps no per-address profile; any native entry stands in for it.
#[cfg(target_arch = "aarch64")]
fn ran_native_guest_code(cpu: &JitCpu) -> bool {
    cpu.stats().native_entries > 0
}

#[cfg(not(target_arch = "aarch64"))]
fn ran_native_guest_code(cpu: &JitCpu) -> bool {
    cpu.profile().iter().any(|(&pc, p)| {
        (GUEST_CODE_BASE..GUEST_CODE_BASE + GUEST_CODE_SIZE).contains(&pc) && p.native_entries > 0
    })
}

#[test]
fn rectangle_guest_code_matches_the_host_handlers_on_every_backend() {
    let (host, _) = emulate(UnicornCpu::new(PARK_ADDR).unwrap(), false);
    let (unicorn, _) = emulate(UnicornCpu::new(PARK_ADDR).unwrap(), true);
    let (interpreted, _) = emulate(JitCpu::new(PARK_ADDR, false).unwrap(), true);
    let (compiled, m) = emulate(jit(), true);
    assert!(
        ran_native_guest_code(&m.cpu),
        "the JIT compiled the guest code"
    );
    for (name, outcome) in [
        ("unicorn", &unicorn),
        ("interpreter", &interpreted),
        ("jit", &compiled),
    ] {
        assert_eq!(outcome.results, host.results, "{name} results");
        assert_eq!(outcome.rects, host.rects, "{name} rectangles");
    }
}

#[cfg(windows)]
#[test]
fn rectangle_guest_code_matches_windows_user32() {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{IntersectRect, IsRectEmpty, SetRectEmpty, UnionRect};
    let (mut rects, ops) = plan();
    let mut native: Vec<RECT> = rects
        .iter()
        .map(|r| RECT {
            left: r[0],
            top: r[1],
            right: r[2],
            bottom: r[3],
        })
        .collect();
    let mut results = vec![0; ops.len()];
    for _ in 0..ITERATIONS {
        for (i, op) in ops.iter().enumerate() {
            let base = native.as_mut_ptr();
            let p = |k: usize| {
                if k == NULL {
                    std::ptr::null_mut()
                } else {
                    // SAFETY: k indexes `native`, which outlives the call.
                    unsafe { base.add(k) }
                }
            };
            // SAFETY: valid or NULL RECT pointers, as the guest passes them.
            results[i] = unsafe {
                match *op {
                    Op::SetRectEmpty(a) => SetRectEmpty(p(a)),
                    Op::IsRectEmpty(a) => IsRectEmpty(p(a)),
                    Op::IntersectRect(a, b, c) => IntersectRect(p(a), p(b), p(c)),
                    Op::UnionRect(a, b, c) => UnionRect(p(a), p(b), p(c)),
                }
            } as u32;
        }
    }
    for (r, n) in rects.iter_mut().zip(&native) {
        *r = [n.left, n.top, n.right, n.bottom];
    }
    let (compiled, _) = emulate(jit(), true);
    assert_eq!(compiled.results, results);
    assert_eq!(compiled.rects, rects);
}

#[test]
fn last_error_and_tls_guest_code_keep_their_teb_contract() {
    fn run<B: Backend>(cpu: B, guest_code: bool) -> Vec<u32> {
        let mut m = machine(cpu, guest_code);
        let f = |m: &mut Machine<B>, name: &str| m.resolve_import("kernel32.dll", name).unwrap();
        let (set, get, alloc, free, set_tls, get_tls) = (
            f(&mut m, "SetLastError"),
            f(&mut m, "GetLastError"),
            f(&mut m, "TlsAlloc"),
            f(&mut m, "TlsFree"),
            f(&mut m, "TlsSetValue"),
            f(&mut m, "TlsGetValue"),
        );
        let slot = DATA + 0x100;
        let out = RESULTS;
        let saved = run_loop(&mut m, |a| {
            let mut n = 0;
            let mut store = |a: &mut Asm| {
                raw(a, &[0xa3], out + 4 * n);
                n += 1;
            };
            a.call(alloc);
            raw(a, &[0xa3], slot); // mov [slot], eax
            a.push_imm(0xabc);
            a.mov_eax_mem(slot).push_eax();
            a.call(set_tls);
            a.push_imm(0x1234);
            a.call(set);
            a.call(get);
            store(a);
            a.mov_eax_mem(slot).push_eax();
            a.call(get_tls);
            store(a);
            a.call(get);
            store(a);
            a.push_imm(77);
            a.call(set);
            a.push_imm(100);
            a.call(get_tls);
            store(a);
            a.call(get);
            store(a);
            a.push_imm(64);
            a.call(get_tls);
            store(a);
            a.mov_eax_mem(slot).push_eax();
            a.call(free);
        });
        assert_eq!(saved[..4], SENTINELS);
        assert_eq!(saved[4], 0);
        (0..6)
            .map(|i| m.cpu.read_u32(out + 4 * i).unwrap())
            .collect()
    }
    let host = run(UnicornCpu::new(PARK_ADDR).unwrap(), false);
    assert_eq!(host[..3], [0x1234, 0xabc, 0]);
    assert_eq!(run(UnicornCpu::new(PARK_ADDR).unwrap(), true), host);
    assert_eq!(run(JitCpu::new(PARK_ADDR, false).unwrap(), true), host);
    assert_eq!(run(jit(), true), host);
}

#[test]
fn fls_guest_code_reads_published_values_and_defers_the_rest_to_the_host() {
    fn run<B: Backend>(cpu: B, guest_code: bool) -> (Vec<u32>, u64) {
        let mut m = machine(cpu, guest_code);
        let f = |m: &mut Machine<B>, name: &str| m.resolve_import("kernel32.dll", name).unwrap();
        let (set, get, alloc, free, set_fls, get_fls) = (
            f(&mut m, "SetLastError"),
            f(&mut m, "GetLastError"),
            f(&mut m, "FlsAlloc"),
            f(&mut m, "FlsFree"),
            f(&mut m, "FlsSetValue"),
            f(&mut m, "FlsGetValue"),
        );
        let slot = DATA + 0x100;
        let out = RESULTS;
        let saved = run_loop(&mut m, |a| {
            let mut n = 0;
            let mut read = |a: &mut Asm, index: Option<u32>| {
                a.push_imm(0x1234);
                a.call(set);
                match index {
                    Some(index) => a.push_imm(index),
                    None => a.mov_eax_mem(slot).push_eax(),
                };
                a.call(get_fls);
                raw(a, &[0xa3], out + 4 * n);
                a.call(get);
                raw(a, &[0xa3], out + 4 * n + 4);
                n += 2;
            };
            a.push_imm(0);
            a.call(alloc);
            raw(a, &[0xa3], slot);
            // Unset, set, out of the copied range, then freed.
            read(a, None);
            a.push_imm(0xabc);
            a.mov_eax_mem(slot).push_eax();
            a.call(set_fls);
            read(a, None);
            read(a, Some(300));
            a.mov_eax_mem(slot).push_eax();
            a.call(free);
            read(a, None);
        });
        assert_eq!(saved[..4], SENTINELS);
        assert_eq!(saved[4], 0);
        let results = (0..8)
            .map(|i| m.cpu.read_u32(out + 4 * i).unwrap())
            .collect();
        (results, m.sys.host_calls)
    }
    let (host, calls) = run(UnicornCpu::new(PARK_ADDR).unwrap(), false);
    assert_eq!(host, [0, 0, 0xabc, 0, 0, 87, 0, 87]);
    // Guest code takes the four SetLastError calls, the four GetLastError
    // calls and the one read of a published value.
    for (results, guest_calls) in [
        run(UnicornCpu::new(PARK_ADDR).unwrap(), true),
        run(JitCpu::new(PARK_ADDR, false).unwrap(), true),
        run(jit(), true),
    ] {
        assert_eq!(results, host);
        assert_eq!(calls - guest_calls, 9 * u64::from(ITERATIONS));
    }
}

const CS: u32 = DATA + 0x200;
const COUNTER: u32 = DATA + 0x240;
const TRIES: u32 = DATA + 0x244;
const HANDLES: u32 = DATA + 0x280;
const THREADS: u32 = 4;
const ROUNDS: u32 = 200;

/// Threads take one section recursively, yield inside it, and update a counter
/// without atomics; TryEnter succeeds for the owner. Every lost update, missed
/// wakeup or broken handoff shows in the counter, the deadline or LockCount.
fn contend<B: Backend>(cpu: B, guest_code: bool, real_time: bool) {
    let clock = if real_time {
        Clock::real()
    } else {
        Clock::virtual_time()
    };
    let mut m = machine_with(cpu, guest_code, clock);
    let f = |m: &mut Machine<B>, name: &str| m.resolve_import("kernel32.dll", name).unwrap();
    let (init, enter, leave, try_enter, sleep, create, wait) = (
        f(&mut m, "InitializeCriticalSection"),
        f(&mut m, "EnterCriticalSection"),
        f(&mut m, "LeaveCriticalSection"),
        f(&mut m, "TryEnterCriticalSection"),
        f(&mut m, "Sleep"),
        f(&mut m, "CreateThread"),
        f(&mut m, "WaitForMultipleObjects"),
    );
    let worker = CODE + 0x8000;
    let mut w = Asm::new(worker);
    w.bytes.push(0x53); // push ebx
    raw(&mut w, &[0xbb], ROUNDS); // mov ebx, ROUNDS
    let top = w.here();
    w.push_imm(CS).call(enter).push_imm(CS).call(enter);
    w.mov_eax_mem(COUNTER).push_eax();
    w.push_imm(0).call(sleep);
    w.bytes.extend_from_slice(&[0x58, 0x40]); // pop eax; inc eax
    raw(&mut w, &[0xa3], COUNTER);
    w.push_imm(CS).call(leave).push_imm(CS).call(try_enter);
    raw(&mut w, &[0x01, 0x05], TRIES); // add [TRIES], eax
    w.push_imm(CS).call(leave).push_imm(CS).call(leave);
    w.bytes.push(0x4b); // dec ebx
    let next = w.here() + 6;
    raw(&mut w, &[0x0f, 0x85], top.wrapping_sub(next)); // jnz top
    w.bytes.push(0x5b); // pop ebx
    raw(&mut w, &[0xb8], 0);
    w.ret_n(4);
    m.cpu.write(worker, &w.bytes).unwrap();

    let mut a = Asm::new(CODE);
    a.push_imm(CS).call(init);
    for i in 0..THREADS {
        for arg in [0, 0, 0, worker, 0, 0] {
            a.push_imm(arg);
        }
        a.call(create);
        raw(&mut a, &[0xa3], HANDLES + 4 * i);
    }
    for arg in [u32::MAX, 1, HANDLES, THREADS] {
        a.push_imm(arg);
    }
    a.call(wait);
    raw(&mut a, &[0xb8], 0);
    a.ret();
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("MAIN", CODE, &[], 0x1_0000).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "threads stopped making progress"
        );
        match m.run_slice(std::time::Duration::from_millis(8)) {
            adv_machine::SliceResult::Complete(RunResult::Exited(0)) => break,
            adv_machine::SliceResult::Complete(RunResult::Idle { .. }) => {
                std::thread::sleep(std::time::Duration::from_millis(1))
            }
            adv_machine::SliceResult::Complete(other) => panic!("{other:?}"),
            adv_machine::SliceResult::Yielded => {}
        }
    }
    let word = |at: u32| m.cpu.read_u32(at).unwrap();
    assert_eq!(
        word(COUNTER),
        THREADS * ROUNDS,
        "updates inside the section"
    );
    assert_eq!(word(TRIES), THREADS * ROUNDS, "TryEnter by the owner");
    assert_eq!(
        [word(CS + 4), word(CS + 8), word(CS + 12)],
        [u32::MAX, 0, 0],
        "LockCount, RecursionCount, OwningThread"
    );
}

#[test]
fn contended_critical_sections_hand_over_between_guest_code_and_host() {
    for guest_code in [false, true] {
        contend(UnicornCpu::new(PARK_ADDR).unwrap(), guest_code, false);
        contend(JitCpu::new(PARK_ADDR, false).unwrap(), guest_code, false);
        contend(jit(), guest_code, false);
    }
}

fn atomics<B: Backend>(cpu: B, guest: bool) -> Vec<u8> {
    let mut m = machine(cpu, guest);
    let names = [
        "InterlockedIncrement",
        "InterlockedDecrement",
        "InterlockedExchange",
        "InterlockedExchangeAdd",
        "InterlockedCompareExchange",
        "InterlockedCompareExchange",
    ];
    let targets: Vec<_> = names
        .iter()
        .map(|n| m.resolve_import("kernel32.dll", n).unwrap())
        .collect();
    let values = [0, 1, u32::MAX, i32::MAX as u32, i32::MIN as u32, 0xdeadbeef];
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let saved = run_loop(&mut m, |a| {
        a.push_imm(0x12345678).call(set_error);
        for (kind, &target) in targets.iter().enumerate() {
            for (i, &value) in values.iter().enumerate() {
                let p = DATA + 0x800 + 4 * (kind * values.len() + i) as u32;
                raw(a, &[0xc7, 0x05], p);
                a.bytes.extend_from_slice(&value.to_le_bytes());
                if kind >= 4 {
                    a.push_imm(if kind == 4 { value } else { value ^ 1 });
                }
                if kind >= 2 {
                    a.push_imm(0x87654321);
                }
                a.push_imm(p).call(target);
                raw(a, &[0xa3], RESULTS + 4 * (kind * values.len() + i) as u32);
            }
        }
        a.call(get_error);
        raw(a, &[0xa3], RESULTS + 0x200);
    });
    assert_eq!(saved[..4], SENTINELS);
    assert_eq!(saved[4], 0);
    let mut bytes = vec![0; 0x5000];
    m.cpu.read(DATA, &mut bytes).unwrap();
    bytes
}

// spec: WIN32-API-16, PROC-MACH-2, PROC-MACH-3
#[test]
fn interlocked_guest_entries_match_host_wraparound_memory_and_abi() {
    assert_eq!(
        atomics(UnicornCpu::new(PARK_ADDR).unwrap(), true),
        atomics(UnicornCpu::new(PARK_ADDR).unwrap(), false)
    );
    assert_eq!(
        atomics(JitCpu::new(PARK_ADDR, false).unwrap(), true),
        atomics(JitCpu::new(PARK_ADDR, false).unwrap(), false)
    );
}

fn atomic_fault(guest: bool, name: &str, pointer: u32) -> (String, u32, bool) {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap(), guest);
    m.cpu.protect(DATA + 0xf000, 4096, Prot::READ).unwrap();
    let target = m.resolve_import("kernel32.dll", name).unwrap();
    let mut code = Asm::new(CODE);
    if name == "InterlockedCompareExchange" {
        code.push_imm(0);
    }
    if !["InterlockedIncrement", "InterlockedDecrement"].contains(&name) {
        code.push_imm(17);
    }
    code.push_imm(pointer).call(target).ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    let context = m.spawn("faulting atomic", CODE, &[], 65536).unwrap();
    let error = m.sys.context(context).teb + adv_machine::layout::teb::LAST_ERROR;
    m.cpu.write_u32(error, 0x12345678).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(
        !matches!(result, RunResult::Exited(0)),
        "invalid atomic must fault"
    );
    let mut tail = vec![0; 4096];
    m.cpu.read(DATA + 0xf000, &mut tail).unwrap();
    let failure = match result {
        RunResult::Failed(error) => format!("{}: {}", error.pc, error.message),
        other => panic!("expected access failure, got {other:?}"),
    };
    (
        failure,
        m.cpu.read_u32(error).unwrap(),
        tail.iter().all(|&v| v == 0),
    )
}
// spec: WIN32-API-16, EXC-SEH-2
#[test]
fn invalid_and_readonly_atomic_words_deliver_the_same_guest_faults() {
    for name in [
        "InterlockedIncrement",
        "InterlockedDecrement",
        "InterlockedExchange",
        "InterlockedExchangeAdd",
        "InterlockedCompareExchange",
    ] {
        for pointer in [0, DATA + 0xf000, DATA + 0xffff] {
            let accelerated = atomic_fault(true, name, pointer);
            assert_eq!(accelerated, atomic_fault(false, name, pointer), "{name}");
            assert_eq!(accelerated.1, 0x12345678);
            assert!(accelerated.2);
        }
    }
}

fn atomic_workers(guest: bool) -> (Vec<u32>, u64) {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap(), guest);
    let targets: Vec<_> = [
        "InterlockedIncrement",
        "InterlockedDecrement",
        "InterlockedExchange",
        "InterlockedExchangeAdd",
        "InterlockedCompareExchange",
        "Sleep",
    ]
    .iter()
    .map(|name| m.resolve_import("kernel32.dll", name).unwrap())
    .collect();
    for thread in 0..4 {
        let base = CODE + thread * 0x1000;
        let mut code = Asm::new(base);
        code.bytes
            .extend_from_slice(&[0x53, 0x56, 0xbe, 64, 0, 0, 0]); // preserve ebx/esi; rounds
        let again = code.here();
        code.push_imm(DATA + 0x800).call(targets[0]);
        code.push_imm(DATA + 0x804).call(targets[1]);
        code.push_imm(thread + 1)
            .push_imm(DATA + 0x808)
            .call(targets[2]);
        code.bytes.push(0x50); // old exchanged word
        code.push_imm(DATA + 0x814).call(targets[3]);
        code.push_imm(2).push_imm(DATA + 0x80c).call(targets[3]);
        let retry = code.here();
        code.mov_eax_mem(DATA + 0x810);
        code.bytes
            .extend_from_slice(&[0x89, 0xc3, 0x50, 0x40, 0x50]); // expected=ebx; push old/new
        code.push_imm(DATA + 0x810).call(targets[4]);
        code.bytes.extend_from_slice(&[0x39, 0xd8]); // cmp eax,ebx
        code.jnz(retry);
        code.push_imm(0).call(targets[5]);
        code.bytes.push(0x4e); // dec esi
        code.jnz(again);
        code.bytes.extend_from_slice(&[0x5e, 0x5b, 0x31, 0xc0]);
        code.ret();
        m.cpu.write(base, &code.bytes).unwrap();
        let ctx = m.spawn("atomic worker", base, &[], 65536).unwrap();
        m.cpu
            .write_u32(
                m.sys.context(ctx).teb + adv_machine::layout::teb::LAST_ERROR,
                0x12345678,
            )
            .unwrap();
    }
    let result = m.run_virtual(10_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let words: Vec<_> = (0..6)
        .map(|i| m.cpu.read_u32(DATA + 0x800 + 4 * i).unwrap())
        .collect();
    assert_eq!(words[0], 256);
    assert_eq!(words[1], 0u32.wrapping_sub(256));
    assert_eq!(words[3], 512);
    assert_eq!(words[4], 256);
    assert_eq!(
        words[2] + words[5],
        640,
        "exchange old returns account for every update"
    );
    for ctx in &m.sys.contexts {
        assert_eq!(
            m.cpu
                .read_u32(ctx.teb + adv_machine::layout::teb::LAST_ERROR)
                .unwrap(),
            0x12345678
        );
    }
    (
        vec![words[0], words[1], words[3], words[4], words[2] + words[5]],
        m.sys.host_calls,
    )
}
// spec: WIN32-API-16
#[test]
fn interlocked_guest_workers_preserve_all_atomic_updates_and_old_returns() {
    let (guest, after) = atomic_workers(true);
    let (host, before) = atomic_workers(false);
    assert_eq!(guest, host);
    assert_eq!(before - after, 1536);
    eprintln!("synthetic atomic workers: non-leaf calls {before} -> {after}");
}

fn crt_bytes<B: Backend>(cpu: B, guest: bool) -> Vec<u8> {
    let mut m = machine(cpu, guest);
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut exports: Vec<_> = registry
        .registered()
        .filter(|(_, name, binding)| {
            binding.guest.is_some()
                && ["memcpy", "memmove", "memset", "memcpy_s", "memmove_s"].contains(name)
        })
        .map(|(module, name, _)| (module.to_owned(), name.to_owned()))
        .collect();
    exports.sort();
    assert_eq!(exports.len(), 22);
    let mut targets = Vec::new();
    for (module, name) in &exports {
        targets.push(m.resolve_import(module, name).unwrap());
    }
    let errno = m.resolve_import("msvcr90.dll", "_errno").unwrap();
    let set_invalid = m
        .resolve_import("msvcr90.dll", "_set_invalid_parameter_handler")
        .unwrap();
    // A returning invalid-parameter handler permits secure error comparisons.
    m.cpu.write(CODE + 0xf000, &[0xc3]).unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let saved = run_loop(&mut m, |a| {
        a.push_imm(0x12345678).call(set_error);
        a.push_imm(CODE + 0xf000).call(set_invalid);
        a.bytes.extend_from_slice(&[0x83, 0xc4, 4]);
        a.call(errno)
            .bytes
            .extend_from_slice(&[0xc7, 0x00, 0x45, 0x23, 0, 0]);
        for (i, ((_, name), &target)) in exports.iter().zip(&targets).enumerate() {
            let base = DATA + 0x1000 + i as u32 * 0x100;
            // Restore an asymmetric source on every iteration.
            for (j, value) in [0x03020100u32, 0x07060504, 0x0b0a0908, 0x0f0e0d0c]
                .into_iter()
                .enumerate()
            {
                raw(a, &[0xc7, 0x05], base + j as u32 * 4);
                a.bytes.extend_from_slice(&value.to_le_bytes());
            }
            let cases = if name.ends_with("_s") {
                vec![
                    vec![base + 32, 31, base, 15],
                    vec![base + 2, 16, base, 11],
                    vec![base, 16, base + 2, 11],
                    vec![0, 0, 0, 0],
                ]
            } else if name == "memset" {
                vec![vec![base + 32, 0x1234ab, 15], vec![0, 0, 0]]
            } else {
                vec![
                    vec![base + 32, base, 15],
                    vec![base + 2, base, 11],
                    vec![base, base + 2, 11],
                    vec![0, 0, 0],
                ]
            };
            for (j, args) in cases.into_iter().enumerate() {
                for arg in args.iter().rev() {
                    a.push_imm(*arg);
                }
                a.call(target).mov_mem_eax(base + 64 + j as u32 * 4);
                a.bytes
                    .extend_from_slice(&[0x83, 0xc4, (args.len() * 4) as u8]);
            }
        }
        // Error cases enter the VC90 handler, preserving its zeroing and errno.
        for (j, name) in ["memcpy_s", "memmove_s"].into_iter().enumerate() {
            let target = targets[exports
                .iter()
                .position(|(module, n)| module == "msvcr90.dll" && n == name)
                .unwrap()];
            for (k, (capacity, source, count)) in
                [(3, DATA + 0x1000, 5), (8, 0, 5)].into_iter().enumerate()
            {
                for arg in [count, source, capacity, DATA + 0x3000] {
                    a.push_imm(arg);
                }
                a.call(target)
                    .mov_mem_eax(DATA + 0x3100 + (j * 16 + k * 8) as u32);
                a.bytes.extend_from_slice(&[0x83, 0xc4, 16]);
                a.call(errno).bytes.extend_from_slice(&[0x8b, 0x00]);
                a.mov_mem_eax(DATA + 0x3104 + (j * 16 + k * 8) as u32);
            }
        }
        a.call(get_error).mov_mem_eax(DATA + 0x3200);
    });
    assert_eq!(&saved[..4], &SENTINELS);
    assert_eq!(saved[4], 0);
    let mut bytes = vec![0; 0x2300];
    m.cpu.read(DATA + 0x1000, &mut bytes).unwrap();
    bytes
}

// spec: WIN32-API-16
#[test]
fn crt_guest_copies_and_fills_match_host_memory_returns_overlap_and_secure_errors() {
    let host = crt_bytes(JitCpu::new(PARK_ADDR, false).unwrap(), false);
    assert_eq!(
        crt_bytes(JitCpu::new(PARK_ADDR, false).unwrap(), true),
        host
    );
    assert_eq!(crt_bytes(JitCpu::new(PARK_ADDR, true).unwrap(), true), host);
    assert_eq!(crt_bytes(UnicornCpu::new(PARK_ADDR).unwrap(), true), host);
}

fn crt_heap<B: Backend>(cpu: B, guest: bool) -> Vec<u8> {
    let mut m = machine(cpu, guest);
    m.sys.stats.enabled = true;
    let mut targets = Vec::new();
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        targets.push((
            m.resolve_import(module, "??2@YAPAXI@Z").unwrap(),
            m.resolve_import(module, "??3@YAXPAX@Z").unwrap(),
            m.resolve_import(module, "_errno").unwrap(),
        ));
    }
    let last_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let saved = run_loop(&mut m, |a| {
        a.push_imm(0x12345678).call(set_error);
        for (i, &(new, delete, errno)) in targets.iter().enumerate() {
            a.call(errno)
                .bytes
                .extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
            for (j, size) in [0, 8, 15, 511, 1024].into_iter().enumerate() {
                a.push_imm(size).call(new);
                a.bytes.extend_from_slice(&[0x83, 0xc4, 4]);
                a.mov_mem_eax(RESULTS + (i * 32 + j * 4) as u32);
                a.bytes.push(0x50);
                a.call(delete);
                a.bytes.extend_from_slice(&[0x83, 0xc4, 4]);
                a.mov_mem_eax(RESULTS + 128 + (i * 32 + j * 4) as u32);
            }
            a.push_imm(0).call(delete);
            a.bytes.extend_from_slice(&[0x83, 0xc4, 4]);
            a.call(errno).bytes.extend_from_slice(&[0x8b, 0x00]);
            a.mov_mem_eax(RESULTS + 272 + i as u32 * 4);
        }
        a.call(last_error).mov_mem_eax(RESULTS + 256);
    });
    assert_eq!(&saved[..4], &SENTINELS);
    assert_eq!(saved[4], 0);
    if guest {
        for &(new, delete, _) in &targets {
            for target in [new, delete] {
                let index =
                    (target - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
                let [entries, fallback, _] = m.sys.thunks.guest_counts(&m.cpu, index);
                assert!(
                    entries > fallback,
                    "heap listing never completed a call: {entries}/{fallback}"
                );
                assert!(fallback > 0, "heap rare-case path was not exercised");
            }
        }
    }
    let mut bytes = vec![0; 284];
    m.cpu.read(RESULTS, &mut bytes).unwrap();
    bytes
}

// spec: WIN32-API-16
#[test]
fn scalar_cpp_guest_heap_matches_host_allocation_free_null_and_large_cases() {
    let host = crt_heap(JitCpu::new(PARK_ADDR, false).unwrap(), false);
    assert_eq!(crt_heap(JitCpu::new(PARK_ADDR, false).unwrap(), true), host);
    assert_eq!(crt_heap(JitCpu::new(PARK_ADDR, true).unwrap(), true), host);
    assert_eq!(crt_heap(UnicornCpu::new(PARK_ADDR).unwrap(), true), host);
}

fn query_call(
    m: &mut Machine<JitCpu>,
    module: &str,
    name: &str,
    args: &[u32],
    leaf: bool,
) -> (u32, bool) {
    use adv_cpu::backend::Reg;
    use adv_machine::{Env, Outcome};
    let entry = m.resolve_import(module, name).unwrap();
    let thunk = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let binding = m.sys.thunks.get(thunk).f;
    m.cpu.set_reg(Reg::Esp, DATA + 512);
    for (i, arg) in args.iter().enumerate() {
        m.cpu.write_u32(DATA + 516 + i as u32 * 4, *arg).unwrap();
    }
    let mut before = vec![0; 4096];
    m.cpu.read(DATA, &mut before).unwrap();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    };
    if leaf {
        if let Some(value) = binding.env_leaf.unwrap()(&mut env) {
            return (value as u32, false);
        }
        let mut after = vec![0; 4096];
        env.cpu.read(DATA, &mut after).unwrap();
        assert_eq!(before, after, "decline wrote guest memory");
    }
    let Outcome::Return(value) = (binding.handler)(&mut env) else {
        panic!("synchronous query")
    };
    (value, leaf)
}

// spec: WIN32-API-15, A64-CTL-12
#[test]
fn errno_leaves_decline_unallocated_cells_then_preserve_module_and_thread_ownership() {
    let mut outcomes = Vec::new();
    for leaf in [true, false] {
        let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap(), true);
        let mut addresses = Vec::new();
        for thread in 0..2 {
            m.sys.current = m.spawn("errno owner", PARK_ADDR, &[], 65536).unwrap();
            let error = m.sys.context(m.sys.current).teb + adv_machine::layout::teb::LAST_ERROR;
            m.cpu.write_u32(error, 0x12345678).unwrap();
            for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll", "ucrtbase.dll"] {
                let (address, declined) = query_call(&mut m, module, "_errno", &[], leaf);
                assert_eq!(declined, leaf);
                assert!(!addresses.contains(&address));
                addresses.push(address);
                m.cpu.write_u32(address, 79 + thread).unwrap();
                assert_eq!(
                    query_call(&mut m, module, "_errno", &[], leaf),
                    (address, false)
                );
                assert_eq!(m.cpu.read_u32(address).unwrap(), 79 + thread);
                assert_eq!(m.cpu.read_u32(error).unwrap(), 0x12345678);
            }
        }
        outcomes.push(addresses);
    }
    assert_eq!(outcomes[0], outcomes[1]);
}

// spec: WIN32-API-15, A64-CTL-12
#[test]
fn resolved_registry_leaves_match_host_returns_outputs_and_errors() {
    use adv_os::registry::{Key, Value, VirtualRegistry};
    fn replay(leaf: bool) -> Vec<u8> {
        let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap(), true);
        m.sys.current = m.spawn("registry owner", PARK_ADDR, &[], 65536).unwrap();
        // An unresolved registry declines without creating the disk-backed owner.
        let entry = m
            .resolve_import("advapi32.dll", "RegQueryValueExW")
            .unwrap();
        let thunk = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let mut env = adv_machine::Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk,
        };
        assert_eq!(
            env.sys.thunks.get(thunk).f.env_leaf.unwrap()(&mut env),
            None
        );
        assert!(m.ext.try_get::<VirtualRegistry>().is_none());
        let mut registry = VirtualRegistry::default();
        registry
            .keys
            .get_mut(&Key {
                root: 0x80000001,
                path: vec![],
            })
            .unwrap()
            .insert(
                vec![],
                Value {
                    kind: 1,
                    display_name: vec![],
                    bytes: vec![65, 0, 66, 0, 0, 0],
                },
            );
        m.ext.insert(registry);
        let mut results = Vec::new();
        for name in ["RegQueryValueExA", "RegQueryValueExW"] {
            for (handle, capacity, data, reserved) in [
                (0x80000001, 32, DATA + 128, 0),
                (0x80000001, 1, DATA + 128, 0),
                (0x80000001, 0, 0, 0),
                (123, 32, DATA + 128, 0),
                (0x80000001, 32, DATA + 128, 1),
                (0x80000001, 32, 0x70000000, 0),
            ] {
                m.cpu.write_u32(DATA + 4, capacity).unwrap();
                let (value, declined) = query_call(
                    &mut m,
                    "advapi32.dll",
                    name,
                    &[handle, 0, reserved, DATA, data, DATA + 4],
                    leaf,
                );
                assert_eq!(
                    declined,
                    leaf && (handle == 123 || reserved != 0 || data == 0x70000000)
                );
                results.extend_from_slice(&value.to_le_bytes());
                let mut bytes = vec![0; 160];
                m.cpu.read(DATA, &mut bytes).unwrap();
                results.extend(bytes);
            }
        }
        results
    }
    assert_eq!(replay(true), replay(false));
}

fn crt_fault(
    guest: bool,
    module: &str,
    name: &str,
    destination: u32,
    source: u32,
) -> (String, u32, Vec<u8>) {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap(), guest);
    let target = m.resolve_import(module, name).unwrap();
    let mut code = Asm::new(CODE);
    let args = if name.ends_with("_s") {
        vec![destination, 8, source, 8]
    } else if name == "memset" {
        vec![destination, 0xab, 8]
    } else {
        vec![destination, source, 8]
    };
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(target).ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.cpu.write(DATA + 0xfffc, &[1, 2, 3, 4]).unwrap();
    m.cpu.protect(DATA + 0xf000, 4096, Prot::READ).unwrap();
    let ctx = m.spawn("CRT fault", CODE, &[], 65536).unwrap();
    let error = m.sys.context(ctx).teb + adv_machine::layout::teb::LAST_ERROR;
    m.cpu.write_u32(error, 0x12345678).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
    let fault = m
        .sys
        .first_cpu_fault
        .as_ref()
        .expect("guest access violation");
    assert!(fault.access().is_some());
    let mut bytes = vec![0; 0x1_0000];
    m.cpu.read(DATA, &mut bytes).unwrap();
    (
        format!("{:?}", fault.fault),
        m.cpu.read_u32(error).unwrap(),
        bytes,
    )
}

// spec: WIN32-API-16, EXC-SEH-2
#[test]
fn crt_guest_and_host_routes_deliver_identical_access_violations_and_partial_writes() {
    let mut registry = Registry::default();
    adv_win32::msvcrt::register(&mut registry);
    for (module, name, binding) in registry.registered() {
        if binding.guest.is_none()
            || !["memcpy", "memmove", "memset", "memcpy_s", "memmove_s"].contains(&name)
        {
            continue;
        }
        for (destination, source) in [
            (DATA + 256, 0x70000000),
            (0x70000000, DATA),
            (DATA + 0xf000, DATA),
            (DATA + 256, DATA + 0xfffc),
            (DATA + 0xeffc, DATA),
        ] {
            if name == "memset" && destination == DATA + 256 {
                continue;
            }
            assert_eq!(
                crt_fault(true, module, name, destination, source),
                crt_fault(false, module, name, destination, source),
                "{module}!{name}"
            );
        }
    }
}
