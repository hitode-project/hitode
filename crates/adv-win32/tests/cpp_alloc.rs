//! Authored guest scalar allocation exercises the common heap and throwing ABI boundary.
use adv_abi::{ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::Heaps;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const NEW: &str = "??2@YAPAXI@Z";
const DELETE: &str = "??3@YAXPAX@Z";
const SIZES: [u32; 4] = [32, 0, 0, 7];

fn observe_live(env: &mut Env<'_>) -> Outcome {
    let heap = env.ext.get::<Heaps>().process_heap;
    let pointers: Vec<_> = (0..4)
        .map(|i| env.cpu.read_u32(DATA + i * 4).unwrap())
        .collect();
    for (i, &pointer) in pointers.iter().enumerate() {
        assert_ne!(pointer, 0);
        assert_eq!(pointer % 8, 0);
        assert!(!pointers[..i].contains(&pointer));
        assert!(env.cpu.accessible(pointer, SIZES[i].max(1), Prot::RW));
        assert_eq!(env.ext.get::<Heaps>().owner(pointer), Some(heap));
        assert_eq!(
            env.ext.get_mut::<Heaps>().size(heap, pointer),
            Some(SIZES[i])
        );
    }
    assert_eq!(env.cpu.read_u32(pointers[0]).unwrap(), 0x1234_5678);
    assert_eq!(env.cpu.read_u32(pointers[0] + 28).unwrap(), 0x8765_4321);
    assert_eq!(
        env.cpu.read_u32(DATA + 32).unwrap(),
        env.cpu.read_u32(DATA + 36).unwrap()
    );
    env.cpu.write_u32(DATA + 48, 1).unwrap();
    Outcome::Return(0)
}
fn observe_freed(env: &mut Env<'_>) -> Outcome {
    for i in 0..4 {
        let pointer = env.cpu.read_u32(DATA + i * 4).unwrap();
        assert!(env.ext.get::<Heaps>().owner(pointer).is_none());
    }
    assert_eq!(env.cpu.read_u32(DATA + 40).unwrap(), 79);
    assert_eq!(adv_win32::util::last_error(env), 73);
    assert_eq!(
        env.cpu.read_u32(DATA + 32).unwrap(),
        env.cpu.read_u32(DATA + 44).unwrap()
    );
    env.cpu.write_u32(DATA + 52, 1).unwrap();
    Outcome::Return(0)
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    registry.add(
        "fixture.dll",
        "live",
        adv_machine::Conv::Cdecl { argc: 0 },
        observe_live,
    );
    registry.add(
        "fixture.dll",
        "freed",
        adv_machine::Conv::Cdecl { argc: 0 },
        observe_freed,
    );
    assert!(registry.signature_conflicts().is_empty());
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for name in [NEW, DELETE] {
            assert_eq!(
                registry.level(module, name),
                Some(ImplementationLevel::Implement)
            );
            assert!(registry.dynamic_available(module, name));
            assert_eq!(
                adv_abi::signature::lookup(module, name).unwrap().conv,
                Some(adv_machine::Conv::Cdecl { argc: 1 })
            );
        }
    }
    assert!(!registry.dynamic_available("ucrtbase.dll", NEW));
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/cpp-alloc",
            "fixture.exe",
            "/nonexistent/cpp-alloc-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn store_esp(code: &mut Asm, address: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn run<B: Backend>(machine: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("C++ scalar allocation", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}
fn success<B: Backend>(cpu: B, module: &str) {
    let mut machine = machine(cpu);
    let new = machine.resolve_import(module, NEW).unwrap();
    let delete = machine.resolve_import(module, DELETE).unwrap();
    let errno = machine.resolve_import(module, "_errno").unwrap();
    let last_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let live = machine.resolve_import("fixture.dll", "live").unwrap();
    let freed = machine.resolve_import("fixture.dll", "freed").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(73).call(last_error);
    code.call(errno).mov_mem_eax(DATA + 28);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
    store_esp(&mut code, DATA + 32);
    for (i, size) in SIZES.into_iter().enumerate() {
        code.push_imm(size)
            .call(new)
            .add_esp(4)
            .mov_mem_eax(DATA + i as u32 * 4);
    }
    store_esp(&mut code, DATA + 36);
    code.mov_eax_mem(DATA);
    code.bytes
        .extend_from_slice(&[0xc7, 0x00, 0x78, 0x56, 0x34, 0x12]);
    code.bytes
        .extend_from_slice(&[0xc7, 0x40, 28, 0x21, 0x43, 0x65, 0x87]);
    code.call(live);
    for i in 0..4 {
        code.mov_eax_mem(DATA + i * 4)
            .push_eax()
            .call(delete)
            .add_esp(4);
    }
    code.push_imm(0).call(delete).add_esp(4);
    store_esp(&mut code, DATA + 44);
    code.mov_eax_mem(DATA + 28)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 40);
    code.call(freed);
    assert!(matches!(run(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(machine.cpu.read_u32(DATA + 48).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 52).unwrap(), 1);
}
#[test]
fn scalar_new_returns_distinct_aligned_real_blocks_and_delete_uses_the_same_heap() {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for jit in [false, true] {
            success(JitCpu::new(PARK_ADDR, jit).unwrap(), module);
        }
        success(UnicornCpu::new(PARK_ADDR).unwrap(), module);
    }
}
fn allocation_failure<B: Backend>(cpu: B, module: &str) {
    let mut machine = machine(cpu);
    let heaps = machine.ext.get_mut::<Heaps>();
    let heap = heaps.process_heap;
    let existing = heaps
        .alloc(&mut machine.cpu, &mut machine.sys.vm, heap, 8, false)
        .unwrap();
    let contents = [0x51; 8];
    machine.cpu.write(existing, &contents).unwrap();
    let new = machine.resolve_import(module, NEW).unwrap();
    let mut code = Asm::new(CODE);
    // Existing common allocator's bounded request limit, without allocating
    // gigabytes or changing the guest's resource table for this test.
    code.push_imm(0x4000_0001)
        .call(new)
        .add_esp(4)
        .mov_mem_imm(DATA, 1);
    let RunResult::Failed(failure) = run(&mut machine, code) else {
        panic!("throwing new continued after failed allocation");
    };
    assert!(
        failure
            .message
            .contains("std::bad_alloc/new-handler dispatch is not implemented"),
        "{}",
        failure.message
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(machine.ext.get::<Heaps>().owner(existing), Some(heap));
    assert_eq!(machine.ext.get_mut::<Heaps>().size(heap, existing), Some(8));
    let mut bytes = [0; 8];
    machine.cpu.read(existing, &mut bytes).unwrap();
    assert_eq!(bytes, contents);
}
#[test]
fn allocation_failure_stops_at_the_unsupported_bad_alloc_boundary_without_null_resume() {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for jit in [false, true] {
            allocation_failure(JitCpu::new(PARK_ADDR, jit).unwrap(), module);
        }
        allocation_failure(UnicornCpu::new(PARK_ADDR).unwrap(), module);
    }
}
