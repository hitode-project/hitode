//! APIs that run as guest code instead of host traps. Every backend runs the same
//! call sequence often enough for the JIT to compile it, with guest code on and
//! off; on Windows the host's own user32 is the reference.

use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::layout::{GUEST_CODE_BASE, GUEST_CODE_SIZE, PARK_ADDR};
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
