//! CRT heap fronts share real allocations and checked in-place resizing.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::Heaps;
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CALLBACK: u32 = CODE + 0x3000;
const ERRNO: u32 = DATA + 0x200;
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Saved(u32),
}
use Arg::{Saved as S, Value as V};
fn machine(jit: bool) -> Machine<JitCpu> {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    adv_win32::msvcrt::register(&mut r);
    assert!(r.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-heap-exports",
            "fixture.exe",
            "/nonexistent/crt-heap-profile",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m
}
fn owner(module: &str) -> &str {
    if module.starts_with("api-ms-win-crt-") {
        "ucrtbase.dll"
    } else {
        module
    }
}
fn invoke(
    m: &mut Machine<JitCpu>,
    a: &mut Asm,
    module: &str,
    name: &str,
    args: &[Arg],
    output: u32,
) {
    for arg in args.iter().rev() {
        match *arg {
            V(v) => {
                a.push_imm(v);
            }
            S(p) => {
                a.mov_eax_mem(p).push_eax();
            }
        }
    }
    a.call(m.resolve_import(module, name).unwrap())
        .add_esp(args.len() as u8 * 4)
        .mov_mem_eax(output);
}
fn store_word(a: &mut Asm, slot: u32, value: u32) {
    a.mov_eax_mem(slot);
    a.bytes.extend_from_slice(&[0xc7, 0]);
    a.bytes.extend_from_slice(&value.to_le_bytes());
}
fn run(m: &mut Machine<JitCpu>, a: Asm) -> RunResult {
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("CRT heap fixture", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn clean(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}

// spec: CRT-LIB-7, CRT-LIB-8
#[test]
fn all_versions_allocate_resize_query_and_release_the_same_heap_objects() {
    for jit in [false, true] {
        for module in [
            "msvcrt20.dll",
            "msvcrt40.dll",
            "msvcrt.dll",
            "msvcr70.dll",
            "msvcr71.dll",
            "msvcr80.dll",
            "msvcr90.dll",
            "msvcr100.dll",
            "msvcr110.dll",
            "msvcr120.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-heap-l1-1-0.dll",
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            invoke(&mut m, &mut a, owner(module), "_errno", &[], ERRNO);
            store_word(&mut a, ERRNO, 79);
            invoke(&mut m, &mut a, module, "malloc", &[V(64)], DATA);
            invoke(&mut m, &mut a, module, "malloc", &[V(64)], DATA + 160);
            store_word(&mut a, DATA, 0x12345678);
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA)], DATA + 4);
            invoke(
                &mut m,
                &mut a,
                module,
                "_expand",
                &[S(DATA), V(32)],
                DATA + 8,
            );
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA)], DATA + 12);
            invoke(
                &mut m,
                &mut a,
                module,
                "_expand",
                &[S(DATA), V(48)],
                DATA + 16,
            );
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA)], DATA + 20);
            // Occupied small-block slots cannot grow to another size bucket.
            // Ordinary realloc can move; expand must fail and retain the block.
            invoke(
                &mut m,
                &mut a,
                module,
                "_expand",
                &[S(DATA), V(512)],
                DATA + 24,
            );
            a.mov_eax_mem(ERRNO)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 28);
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA)], DATA + 32);
            a.mov_eax_mem(DATA)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 164);
            invoke(
                &mut m,
                &mut a,
                module,
                "_expand",
                &[S(DATA), V(0)],
                DATA + 36,
            );
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA)], DATA + 40);
            invoke(&mut m, &mut a, module, "free", &[S(DATA)], DATA + 44);
            invoke(&mut m, &mut a, module, "free", &[S(DATA + 160)], DATA + 168);
            invoke(&mut m, &mut a, module, "calloc", &[V(4), V(4)], DATA + 48);
            a.mov_eax_ptr_eax().mov_mem_eax(DATA + 52);
            store_word(&mut a, DATA + 48, 0xdead1234);
            invoke(
                &mut m,
                &mut a,
                module,
                "realloc",
                &[S(DATA + 48), V(32)],
                DATA + 56,
            );
            a.mov_eax_ptr_eax().mov_mem_eax(DATA + 60);
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA + 56)], DATA + 64);
            invoke(
                &mut m,
                &mut a,
                module,
                "realloc",
                &[S(DATA + 56), V(0)],
                DATA + 68,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                "calloc",
                &[V(u32::MAX), V(2)],
                DATA + 72,
            );
            invoke(&mut m, &mut a, module, "malloc", &[V(0)], DATA + 76);
            invoke(&mut m, &mut a, module, "malloc", &[V(0)], DATA + 80);
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA + 76)], DATA + 84);
            invoke(
                &mut m,
                &mut a,
                module,
                "calloc",
                &[V(0), V(u32::MAX)],
                DATA + 88,
            );
            invoke(&mut m, &mut a, module, "_msize", &[S(DATA + 88)], DATA + 92);
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            let result = run(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            let word = |p| m.cpu.read_u32(DATA + p).unwrap();
            let pointer = word(0);
            assert_ne!(pointer, 0);
            assert_eq!(word(4), 64);
            assert_eq!(word(8), pointer);
            assert_eq!(word(12), 32);
            assert_eq!(word(16), pointer);
            assert_eq!(word(20), 48);
            assert_eq!(word(24), 0);
            assert_eq!(word(28), 12);
            assert_eq!(word(32), 48);
            assert_eq!(word(164), 0x12345678);
            assert_eq!(word(36), pointer);
            assert_eq!(word(40), 1);
            assert_eq!(word(52), 0);
            assert_ne!(word(56), 0);
            assert_eq!(word(60), 0xdead1234);
            assert_eq!(word(64), 32);
            assert_eq!(word(68), 0);
            assert_eq!(word(72), 0);
            let heap = m.ext.get::<Heaps>().process_heap;
            let empty = [word(76), word(80), word(88)];
            for p in empty {
                assert_ne!(p, 0);
                assert_eq!(m.ext.get::<Heaps>().owner(&m.cpu, p), Some(heap));
            }
            assert_ne!(empty[0], empty[1]);
            assert_ne!(empty[0], empty[2]);
            assert_ne!(empty[1], empty[2]);
            assert_eq!(word(84), 1);
            assert_eq!(word(92), 1);
            clean(&m);
        }
    }
}

// spec: CRT-LIB-8
#[test]
fn null_size_queries_and_expansion_use_the_runtime_invalid_parameter_profile() {
    for jit in [false, true] {
        for (module, modern) in [
            ("msvcr70.dll", false),
            ("msvcr90.dll", true),
            ("ucrtbase.dll", true),
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            if modern {
                let mut callback = Asm::new(CALLBACK);
                callback.bytes.extend_from_slice(&[0xff, 0x05]);
                callback
                    .bytes
                    .extend_from_slice(&(DATA + 128).to_le_bytes());
                callback.ret();
                m.cpu.write(CALLBACK, &callback.bytes).unwrap();
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_set_invalid_parameter_handler",
                    &[V(CALLBACK)],
                    DATA + 120,
                );
            }
            invoke(&mut m, &mut a, module, "_msize", &[V(0)], DATA);
            invoke(&mut m, &mut a, module, "_expand", &[V(0), V(16)], DATA + 4);
            invoke(&mut m, &mut a, module, "_errno", &[], ERRNO);
            a.mov_eax_ptr_eax().mov_mem_eax(DATA + 8);
            invoke(&mut m, &mut a, module, "free", &[V(0)], DATA + 12);
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            let result = run(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module}: {result:?}"
            );
            assert_eq!(m.cpu.read_u32(DATA).unwrap(), u32::MAX);
            assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 22);
            assert_eq!(
                m.cpu.read_u32(DATA + 128).unwrap(),
                if modern { 2 } else { 0 }
            );
            clean(&m);
        }
    }
}

#[path = "crt_heap_exports/aligned.rs"]
mod aligned;
