//! Authored process-heap programs exercise shared lists with and without host traps.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Handler, Machine, Outcome, Resolver, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Heaps, Loader, Modules, Process};
use adv_pe::ImportName;
use adv_win32::{Win32, Win32Config};
use std::{collections::BTreeSet, sync::OnceLock, time::Duration};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x420000;
static ALLOCATE: OnceLock<Handler> = OnceLock::new();
static FREE: OnceLock<Handler> = OnceLock::new();
#[derive(Default)]
struct Calls {
    allocate: usize,
    free: usize,
}
fn allocate(env: &mut Env<'_>) -> Outcome {
    env.ext.get_or_default::<Calls>().allocate += 1;
    ALLOCATE.get().unwrap()(env)
}
fn free(env: &mut Env<'_>) -> Outcome {
    env.ext.get_or_default::<Calls>().free += 1;
    FREE.get().unwrap()(env)
}
fn machine(jit: bool, direct: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    for (name, saved, wrapper) in [
        ("RtlAllocateHeap", &ALLOCATE, allocate as Handler),
        ("RtlFreeHeap", &FREE, free as Handler),
    ] {
        let original = registry.host_fn("ntdll.dll", name).unwrap();
        let _ = saved.set(original.handler);
        registry.add("ntdll.dll", name, original.conv, wrapper);
        registry.set_guest("ntdll.dll", name, original.guest.unwrap());
    }
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new("/nonexistent/heap", "heap.exe", "/nonexistent/heap-data"),
    )
    .unwrap();
    m.sys.thunks.set_guest_code(&mut m.cpu, direct).unwrap();
    m.cpu.map(CODE, 0x20000, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
    m.ext.insert(Calls::default());
    m
}
struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}
fn api(m: &mut Machine<JitCpu>, module: &str, name: &str) -> Result<u32, String> {
    let index = m.load(&Empty, module).map_err(|e| format!("{e:?}"))?;
    Loader::new(&mut m.cpu, &mut m.sys, m.ext.get_mut::<Modules>(), &Empty)
        .resolve_dynamic(
            index,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .map(|e| e.0)
        .map_err(|e| format!("{e:?}"))
}
fn seed(m: &mut Machine<JitCpu>, size: u32) -> u32 {
    let h = m.ext.get_mut::<Heaps>();
    let p = h.alloc(&mut m.cpu, &mut m.sys.vm, 0, size, false).unwrap();
    m.cpu.write(p, &vec![0xa5; size as usize]).unwrap();
    assert!(h.free(&mut m.cpu, &mut m.sys.vm, 0, p));
    p
}
fn run(m: &mut Machine<JitCpu>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("heap", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(10_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}
fn invoke(code: &mut Asm, api: u32, args: &[u32], result: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(api).mov_mem_eax(result);
}
#[test]
// spec: WIN32-OBJECT-9
fn every_small_size_uses_guest_lists_and_host_size_free_preserve_zeroing() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        for class in 1..=64 {
            seed(&mut m, class * 8);
        }
        let heap = m.ext.get::<Heaps>().process_heap;
        let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
        let rtl = api(&mut m, "ntdll.dll", "RtlAllocateHeap").unwrap();
        assert_eq!(alloc, rtl);
        let mut code = Asm::new(CODE);
        for size in 1..=512 {
            invoke(&mut code, alloc, &[heap, 8, size], DATA + size * 4);
        }
        run(&mut m, code);
        assert_eq!(m.ext.get::<Calls>().allocate, 0);
        let mut live = BTreeSet::new();
        for size in 1..=512 {
            let p = m.cpu.read_u32(DATA + size * 4).unwrap();
            assert!(live.insert(p));
            assert_eq!(m.ext.get::<Heaps>().size(&m.cpu, heap, p), Some(size));
            let mut bytes = vec![1; size as usize];
            m.cpu.read(p, &mut bytes).unwrap();
            assert!(bytes.iter().all(|&b| b == 0));
            assert!(
                m.ext
                    .get_mut::<Heaps>()
                    .free(&mut m.cpu, &mut m.sys.vm, heap, p)
            );
        }
        assert!(m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
        let p = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, heap, 7, false)
            .unwrap();
        let check = m.cpu.read_u32(p - 4).unwrap();
        m.cpu.write_u32(p - 4, check ^ 1).unwrap();
        assert_eq!(m.ext.get::<Heaps>().size(&m.cpu, heap, p), None);
        assert!(
            !m.ext
                .get_mut::<Heaps>()
                .free(&mut m.cpu, &mut m.sys.vm, heap, p)
        );
        m.cpu.write_u32(p - 4, check).unwrap();
        assert!(
            m.ext
                .get_mut::<Heaps>()
                .free(&mut m.cpu, &mut m.sys.vm, heap, p)
        );
    }
}
#[test]
// spec: WIN32-OBJECT-9
fn host_blocks_guest_free_and_invalid_frees_preserve_host_results_and_last_error() {
    for jit in [false, true] {
        let mut results = Vec::new();
        for direct in [false, true] {
            let mut m = machine(jit, direct);
            let heap = m.ext.get::<Heaps>().process_heap;
            let p = m
                .ext
                .get_mut::<Heaps>()
                .alloc(&mut m.cpu, &mut m.sys.vm, heap, 19, false)
                .unwrap();
            let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
            assert_eq!(free, api(&mut m, "ntdll.dll", "RtlFreeHeap").unwrap());
            let set = api(&mut m, "kernel32.dll", "SetLastError").unwrap();
            let get = api(&mut m, "kernel32.dll", "GetLastError").unwrap();
            let mut code = Asm::new(CODE);
            invoke(&mut code, set, &[0x1234], DATA + 100);
            for (i, pointer) in [p + 1, p + 8, 0xdead0000, p, p, 0].into_iter().enumerate() {
                invoke(&mut code, free, &[heap, 0, pointer], DATA + i as u32 * 8);
                invoke(&mut code, get, &[], DATA + i as u32 * 8 + 4);
            }
            m.cpu.write(DATA + 0x8000, b"x\0").unwrap();
            let format = api(&mut m, "kernel32.dll", "FormatMessageA").unwrap();
            invoke(
                &mut code,
                format,
                &[0x500, DATA + 0x8000, 0, 0, DATA + 0x8010, 0, 0],
                DATA + 0x8020,
            );
            code.mov_eax_mem(DATA + 0x8010)
                .push_eax()
                .push_imm(0)
                .push_imm(heap)
                .call(free)
                .mov_mem_eax(DATA + 0x8024);
            run(&mut m, code);
            assert_eq!(m.cpu.read_u32(DATA + 0x8020).unwrap(), 1);
            assert_eq!(m.cpu.read_u32(DATA + 0x8024).unwrap(), 1);
            let formatted = m.cpu.read_u32(DATA + 0x8010).unwrap();
            assert!(
                m.ext
                    .get::<adv_os::global::Globals>()
                    .from_allocation(&m.sys.objects, formatted)
                    .is_none()
            );
            results.push(
                (0..12)
                    .map(|i| m.cpu.read_u32(DATA + i * 4).unwrap())
                    .collect::<Vec<_>>(),
            );
            assert_eq!(m.ext.get::<Calls>().free, if direct { 6 } else { 7 });
            assert!(m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(
            results[0],
            [
                0, 0x1234, 0, 0x1234, 0, 0x1234, 1, 0x1234, 0, 0x1234, 1, 0x1234
            ]
        );
    }
}
#[test]
// spec: WIN32-OBJECT-9
fn preempted_guest_threads_recycle_without_missing_or_duplicate_blocks() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        seed(&mut m, 24);
        let heap = m.ext.get::<Heaps>().process_heap;
        let free_blocks = |m: &Machine<JitCpu>| {
            let mut blocks = BTreeSet::new();
            let mut pointer = m.cpu.read_u32(heap + 0x110 + 2 * 8).unwrap();
            while pointer != 0 {
                assert!(blocks.insert(pointer), "duplicate free block");
                pointer = m.cpu.read_u32(pointer + 4).unwrap();
            }
            blocks
        };
        let expected = free_blocks(&m);
        let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
        let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
        m.set_scheduling_quantum(Some(Duration::from_micros(1)))
            .unwrap();
        for i in 0..2 {
            let base = CODE + i * 0x1000;
            let mut code = Asm::new(base);
            code.bytes.extend_from_slice(&[0x53, 0xbb]);
            code.bytes.extend_from_slice(&2000u32.to_le_bytes());
            let top = code.here();
            invoke(&mut code, alloc, &[heap, 0, 24], DATA + i * 4);
            code.push_eax().push_imm(0).push_imm(heap).call(free);
            code.bytes.extend_from_slice(&[0x4b, 0x0f, 0x85]);
            let next = code.here() + 4;
            code.bytes
                .extend_from_slice(&top.wrapping_sub(next).to_le_bytes());
            code.bytes.push(0x5b);
            code.bytes.extend_from_slice(&[0x31, 0xc0]);
            code.ret();
            m.cpu.write(base, &code.bytes).unwrap();
            m.spawn("heap worker", base, &[], 65536).unwrap();
        }
        let result = m.run_virtual(10_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(m.ext.get::<Calls>().allocate, 0);
        assert_eq!(m.ext.get::<Calls>().free, 0);
        assert_eq!(m.cpu.read_u32(heap + 0x108).unwrap(), 0);
        assert!(m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
        assert_eq!(free_blocks(&m), expected);
    }
}
#[test]
// spec: WIN32-OBJECT-9
fn heap_lock_routes_guest_calls_through_the_host_and_walk_reports_free_blocks() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        seed(&mut m, 40);
        // A grown arena and its first ordinary block can share the same address.
        for _ in 0..3 {
            m.ext
                .get_mut::<Heaps>()
                .alloc(&mut m.cpu, &mut m.sys.vm, 0, 0x60000, false)
                .unwrap();
        }
        let heap = m.ext.get::<Heaps>().process_heap;
        let lock = api(&mut m, "kernel32.dll", "HeapLock").unwrap();
        let unlock = api(&mut m, "kernel32.dll", "HeapUnlock").unwrap();
        let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
        let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
        let validate = api(&mut m, "kernel32.dll", "HeapValidate").unwrap();
        let walk = api(&mut m, "kernel32.dll", "HeapWalk").unwrap();
        let mut code = Asm::new(CODE);
        invoke(&mut code, lock, &[heap], DATA);
        invoke(&mut code, alloc, &[heap, 0, 40], DATA + 4);
        code.push_eax()
            .push_imm(0)
            .push_imm(heap)
            .call(free)
            .mov_mem_eax(DATA + 8);
        invoke(&mut code, validate, &[heap, 0, 0], DATA + 12);
        let top = code.here();
        invoke(&mut code, walk, &[heap, DATA + 64], DATA + 16);
        code.bytes.extend_from_slice(&[0x85, 0xc0, 0x0f, 0x85]);
        let next = code.here() + 4;
        code.bytes
            .extend_from_slice(&top.wrapping_sub(next).to_le_bytes());
        let get = api(&mut m, "kernel32.dll", "GetLastError").unwrap();
        invoke(&mut code, get, &[], DATA + 20);
        invoke(&mut code, unlock, &[heap], DATA + 24);
        run(&mut m, code);
        for offset in [0, 8, 12, 24] {
            assert_eq!(m.cpu.read_u32(DATA + offset).unwrap(), 1);
        }
        assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 259);
        assert_eq!(m.ext.get::<Calls>().allocate, 1);
        assert_eq!(m.ext.get::<Calls>().free, 1);
        let h = m.ext.get::<Heaps>();
        let mut after = 0;
        let mut after_flags = 0;
        let mut small_free = BTreeSet::new();
        while let Some(row) = h.walk(&m.cpu, &m.sys.vm, heap, after, after_flags).unwrap() {
            after = u32::from_le_bytes(row[..4].try_into().unwrap());
            after_flags = u16::from_le_bytes(row[10..12].try_into().unwrap());
            if row[8] == 8 && row[10] == 0 {
                assert!(small_free.insert(after));
            }
        }
        assert!(small_free.len() >= 0x4000 / 48);
        let free = *small_free.first().unwrap();
        m.cpu.write_u32(free + 4, free).unwrap();
        assert!(!m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
        assert!(
            m.ext
                .get::<Heaps>()
                .walk(&m.cpu, &m.sys.vm, heap, 0, 0)
                .is_err()
        );
    }
}

fn private_pair(code: &mut Asm, alloc: u32, free: u32, handle: u32, output: u32) {
    code.push_imm(4)
        .push_imm(0)
        .mov_eax_mem(handle)
        .push_eax()
        .call(alloc)
        .mov_mem_eax(output);
    code.push_eax()
        .push_imm(0)
        .mov_eax_mem(handle)
        .push_eax()
        .call(free)
        .mov_mem_eax(output + 4);
}

#[test]
// spec: WIN32-OBJECT-9
fn growable_private_heaps_avoid_host_calls_after_refill() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        let create = api(&mut m, "kernel32.dll", "HeapCreate").unwrap();
        let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
        let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
        let mut code = Asm::new(CODE);
        for (i, options) in [0, 1, 0x40000, 0x40001].into_iter().enumerate() {
            let data = DATA + i as u32 * 16;
            invoke(&mut code, create, &[options, 0x1000, 0], data);
            for _ in 0..64 {
                private_pair(&mut code, alloc, free, data, data + 4);
            }
        }
        run(&mut m, code);
        assert_eq!(
            m.ext.get::<Calls>().allocate,
            4,
            "only the first refill of each heap enters the host"
        );
        assert_eq!(m.ext.get::<Calls>().free, 0);
        let root = m.ext.get::<Heaps>().process_heap;
        assert_eq!(m.cpu.read_u32(root + 0xb14).unwrap(), 0);
        for i in 0..4 {
            let heap = m.cpu.read_u32(DATA + i * 16).unwrap();
            let pointer = m.cpu.read_u32(DATA + i * 16 + 4).unwrap();
            assert_ne!(heap, 0);
            assert_ne!(pointer, 0);
            assert_eq!(m.cpu.read_u32(DATA + i * 16 + 8).unwrap(), 1);
            assert_eq!(m.cpu.accessible(pointer, 4, Prot::EXEC), i >= 2);
            assert_eq!(m.cpu.read_u32(heap + 0x108).unwrap(), 0);
            assert!(m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
        }
    }
}

#[test]
// spec: WIN32-OBJECT-9
fn fixed_and_exception_heaps_retain_host_calls() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        let create = api(&mut m, "kernel32.dll", "HeapCreate").unwrap();
        let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
        let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
        let mut code = Asm::new(CODE);
        for (i, (options, maximum)) in [(0, 0x10000), (4, 0), (2, 0)].into_iter().enumerate() {
            let data = DATA + i as u32 * 16;
            invoke(&mut code, create, &[options, 0x1000, maximum], data);
            for _ in 0..3 {
                private_pair(&mut code, alloc, free, data, data + 4);
            }
        }
        run(&mut m, code);
        assert_eq!(m.ext.get::<Calls>().allocate, 9);
        assert_eq!(m.ext.get::<Calls>().free, 9);
        for i in 0..3 {
            assert_ne!(m.cpu.read_u32(DATA + i * 16 + 4).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + i * 16 + 8).unwrap(), 1);
        }
    }
}

#[test]
// spec: WIN32-OBJECT-9
fn private_heap_registry_capacity_keeps_excess_heaps_usable() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        let create = api(&mut m, "kernel32.dll", "HeapCreate").unwrap();
        let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
        let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
        let mut code = Asm::new(CODE);
        for i in 0..17 {
            let data = DATA + i * 16;
            invoke(&mut code, create, &[0, 0x1000, 0], data);
            for _ in 0..3 {
                private_pair(&mut code, alloc, free, data, data + 4);
            }
        }
        run(&mut m, code);
        assert_eq!(m.ext.get::<Calls>().allocate, 15 + 2 * 3);
        assert_eq!(m.ext.get::<Calls>().free, 2 * 3);
        let root = m.ext.get::<Heaps>().process_heap;
        let registered: BTreeSet<_> = (0..16)
            .map(|i| m.cpu.read_u32(root + 0xb20 + i * 4).unwrap())
            .collect();
        assert_eq!(registered.len(), 16);
        assert!(registered.contains(&root));
        for i in 0..17 {
            let heap = m.cpu.read_u32(DATA + i * 16).unwrap();
            assert_ne!(m.cpu.read_u32(DATA + i * 16 + 4).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + i * 16 + 8).unwrap(), 1);
            assert_eq!(registered.contains(&heap), i < 15);
        }
    }
}

#[test]
// spec: WIN32-OBJECT-9
fn destroyed_private_heap_handles_fall_back_and_reuse_has_fresh_slabs() {
    for jit in [false, true] {
        for direct in [false, true] {
            let mut m = machine(jit, direct);
            let create = api(&mut m, "kernel32.dll", "HeapCreate").unwrap();
            let destroy = api(&mut m, "kernel32.dll", "HeapDestroy").unwrap();
            let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
            let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
            let mut code = Asm::new(CODE);
            invoke(&mut code, create, &[0, 0x1000, 0], DATA);
            private_pair(&mut code, alloc, free, DATA, DATA + 4);
            code.mov_eax_mem(DATA)
                .push_eax()
                .call(destroy)
                .mov_mem_eax(DATA + 12);
            code.push_imm(4)
                .push_imm(0)
                .mov_eax_mem(DATA)
                .push_eax()
                .call(alloc)
                .mov_mem_eax(DATA + 16);
            invoke(&mut code, create, &[0, 0x1000, 0], DATA + 20);
            private_pair(&mut code, alloc, free, DATA + 20, DATA + 24);
            private_pair(&mut code, alloc, free, DATA + 20, DATA + 24);
            run(&mut m, code);
            let heap = m.cpu.read_u32(DATA).unwrap();
            assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 1);
            assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), heap);
            assert_ne!(m.cpu.read_u32(DATA + 24).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + 28).unwrap(), 1);
            assert_eq!(m.ext.get::<Calls>().allocate, if direct { 3 } else { 4 });
            assert_eq!(m.ext.get::<Calls>().free, if direct { 0 } else { 3 });
            assert_eq!(m.cpu.read_u32(heap + 0x10c).unwrap(), 1);
            assert!(m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
            let root = m.ext.get::<Heaps>().process_heap;
            assert_eq!(
                (0..16)
                    .filter(|i| m.cpu.read_u32(root + 0xb20 + i * 4).unwrap() == heap)
                    .count(),
                1
            );
        }
    }
}

#[test]
// spec: WIN32-OBJECT-9
fn accounted_private_blocks_keep_the_host_free_boundary() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        let heaps = m.ext.get_mut::<Heaps>();
        let heap = heaps.create(&mut m.cpu, &mut m.sys.vm).unwrap();
        assert!(heaps.enable_small_heap(&mut m.cpu, heap).unwrap());
        let pointer = heaps
            .alloc(&mut m.cpu, &mut m.sys.vm, heap, 4, false)
            .unwrap();
        heaps.account_small(&mut m.cpu, pointer);
        let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
        let mut code = Asm::new(CODE);
        invoke(&mut code, free, &[heap, 0, pointer], DATA);
        run(&mut m, code);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
        assert_eq!(m.ext.get::<Calls>().free, 1);
        assert_eq!(m.ext.get::<Heaps>().size(&m.cpu, heap, pointer), None);
        assert!(m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
    }
}

#[test]
// spec: WIN32-OBJECT-9
fn live_small_block_bursts_leave_room_for_other_guest_size_classes() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        let heap = m.ext.get::<Heaps>().process_heap;
        let mut last = 0;
        for _ in 0..70_000 {
            last = m
                .ext
                .get_mut::<Heaps>()
                .alloc(&mut m.cpu, &mut m.sys.vm, heap, 32, false)
                .unwrap();
        }
        assert!(m.cpu.read_u32(heap + 0x10c).unwrap() < 16);
        let warm = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, heap, 4, false)
            .unwrap();
        assert!(
            m.ext
                .get_mut::<Heaps>()
                .free(&mut m.cpu, &mut m.sys.vm, heap, warm)
        );
        m.cpu.write_u32(heap + 0xb18, u32::MAX).unwrap();
        let alloc = api(&mut m, "kernel32.dll", "HeapAlloc").unwrap();
        let free = api(&mut m, "kernel32.dll", "HeapFree").unwrap();
        m.cpu.write_u32(DATA, heap).unwrap();
        let mut code = Asm::new(CODE);
        for _ in 0..64 {
            private_pair(&mut code, alloc, free, DATA, DATA + 4);
        }
        invoke(&mut code, free, &[heap, 0, last], DATA + 12);
        run(&mut m, code);
        assert_eq!(m.ext.get::<Calls>().allocate, 0);
        assert_eq!(m.ext.get::<Calls>().free, 0);
        assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 1);
        assert!(m.ext.get::<Heaps>().validate(&m.cpu, heap, 0));
    }
}

#[test]
// spec: WIN32-OBJECT-9
fn live_small_blocks_replenish_without_filling_the_slab_table() {
    let mut m = machine(true, true);
    let heaps = m.ext.get_mut::<Heaps>();
    let heap = heaps.process_heap;
    // 32 MiB of live blocks across three size classes stay on slabs.
    for _ in 0..(32 << 20) / 3 / 256 {
        for size in [32, 100, 500] {
            let pointer = heaps
                .alloc(&mut m.cpu, &mut m.sys.vm, heap, size, false)
                .unwrap();
            assert!(heaps.owner(&m.cpu, pointer).is_some());
        }
    }
    let slabs = m.cpu.read_u32(heap + 0x10c).unwrap();
    assert!(slabs <= 24, "{slabs} slabs");
}
