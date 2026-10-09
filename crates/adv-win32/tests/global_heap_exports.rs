//! Authored Global/Local owner retirement and live RTL routing checks.
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Loader, Modules, Process, global::Globals};
use adv_pe::ImportName;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}

fn machine(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/global-heap-probe",
            "probe.exe",
            "/nonexistent/global-heap-probe-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn rtl_hook(machine: &mut Machine<JitCpu>, native: usize, index: u32, name: &str) {
    let target = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    )
    .resolve_dynamic(
        native,
        &ImportName::Name {
            hint: 0,
            name: name.into(),
        },
    )
    .unwrap()
    .0;
    let hook = CODE + 0x800 + index * 0x80;
    let original = hook + 0x30;
    let mut bytes = [0; 32];
    machine.cpu.read(target, &mut bytes).unwrap();
    machine.cpu.write(original, &bytes).unwrap();
    let mut code = vec![0xff, 0x05]; // inc dword ptr [counter]; jmp original
    code.extend((DATA + 0x100 + index * 4).to_le_bytes());
    code.push(0xe9);
    code.extend(original.wrapping_sub(hook + 11).to_le_bytes());
    machine.cpu.write(hook, &code).unwrap();
    let mut jump = vec![0xe9];
    jump.extend(hook.wrapping_sub(target + 5).to_le_bytes());
    machine.cpu.write(target, &jump).unwrap();
}

fn execute(machine: &mut Machine<JitCpu>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.start_main(CODE, CODE, 65536, "probe.exe").unwrap();
    let result = machine.run_virtual(1000000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

#[test]
fn first_global_alloc_and_fixed_movable_realloc_size_free_reach_rtl_hooks() {
    for jit in [false, true] {
        let mut machine = machine(jit);
        let native = machine.load(&Empty, "ntdll.dll").unwrap();
        for (index, name) in [
            "RtlAllocateHeap",
            "RtlReAllocateHeap",
            "RtlSizeHeap",
            "RtlFreeHeap",
        ]
        .iter()
        .enumerate()
        {
            rtl_hook(&mut machine, native, index as u32, name);
        }
        let alloc = machine
            .resolve_import("kernel32.dll", "GlobalAlloc")
            .unwrap();
        let size = machine
            .resolve_import("kernel32.dll", "GlobalSize")
            .unwrap();
        let realloc = machine
            .resolve_import("kernel32.dll", "GlobalReAlloc")
            .unwrap();
        let free = machine
            .resolve_import("kernel32.dll", "GlobalFree")
            .unwrap();
        let mut code = Asm::new(CODE);
        for (index, flags) in [0, 2].into_iter().enumerate() {
            let handle = DATA + index as u32 * 32;
            code.push_imm(32)
                .push_imm(flags)
                .call(alloc)
                .mov_mem_eax(handle);
            code.push_eax().call(size).mov_mem_eax(handle + 4);
            code.push_imm(2)
                .push_imm(48)
                .mov_eax_mem(handle)
                .push_eax()
                .call(realloc)
                .mov_mem_eax(handle + 8);
            code.push_eax().call(size).mov_mem_eax(handle + 12);
            code.mov_eax_mem(handle + 8)
                .push_eax()
                .call(free)
                .mov_mem_eax(handle + 16);
        }
        execute(&mut machine, code);
        for index in 0..2 {
            let at = DATA + index * 32;
            assert_eq!(machine.cpu.read_u32(at + 4).unwrap(), 32);
            assert_eq!(machine.cpu.read_u32(at + 12).unwrap(), 48);
            assert_eq!(machine.cpu.read_u32(at + 16).unwrap(), 0);
            let handle = machine.cpu.read_u32(at + 8).unwrap();
            assert!(
                machine
                    .ext
                    .get::<Globals>()
                    .get(&machine.sys.objects, handle)
                    .is_none()
            );
        }
        let counts: Vec<_> = (0..4)
            .map(|index| machine.cpu.read_u32(DATA + 0x100 + index * 4).unwrap())
            .collect();
        assert_eq!(counts, [2, 2, 4, 2]);
        assert!(
            machine
                .ext
                .get_or_default::<ApiDiagnostics>()
                .acceptance_eligible(RunMode::Strict)
        );
    }
}

#[test]
fn rtl_and_crt_free_realloc_retire_fixed_global_aliases() {
    for jit in [false, true] {
        for crt in [false, true] {
            let mut machine = machine(jit);
            let native = machine.load(&Empty, "ntdll.dll").unwrap();
            let alloc = machine
                .resolve_import("kernel32.dll", "GlobalAlloc")
                .unwrap();
            let free = if crt {
                machine.resolve_import("msvcrt.dll", "free").unwrap()
            } else {
                Loader::new(
                    &mut machine.cpu,
                    &mut machine.sys,
                    machine.ext.get_mut::<Modules>(),
                    &Empty,
                )
                .resolve_dynamic(
                    native,
                    &ImportName::Name {
                        hint: 0,
                        name: "RtlFreeHeap".into(),
                    },
                )
                .unwrap()
                .0
            };
            let realloc = machine.resolve_import("msvcrt.dll", "realloc").unwrap();
            let heap = machine.ext.get::<adv_os::Heaps>().process_heap;
            let mut code = Asm::new(CODE);
            code.push_imm(32).push_imm(0).call(alloc).mov_mem_eax(DATA);
            if crt {
                code.push_imm(48)
                    .push_eax()
                    .call(realloc)
                    .add_esp(8)
                    .mov_mem_eax(DATA + 4);
                code.push_eax().call(free).add_esp(4);
            } else {
                code.push_eax()
                    .push_imm(0)
                    .push_imm(heap)
                    .call(free)
                    .mov_mem_eax(DATA + 8);
            }
            execute(&mut machine, code);
            let old = machine.cpu.read_u32(DATA).unwrap();
            assert!(
                machine
                    .ext
                    .get::<Globals>()
                    .get(&machine.sys.objects, old)
                    .is_none()
            );
            if crt {
                let new = machine.cpu.read_u32(DATA + 4).unwrap();
                assert_ne!(new, 0);
                assert!(
                    machine
                        .ext
                        .get::<Globals>()
                        .get(&machine.sys.objects, new)
                        .is_none()
                );
            } else {
                assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 1);
            }
        }
    }
}

#[test]
fn large_heap_exports_zero_freed_storage_without_zero_memory() {
    for jit in [false, true] {
        for moving in [false, true] {
            let mut machine = machine(jit);
            let alloc = machine.resolve_import("kernel32.dll", "HeapAlloc").unwrap();
            let realloc = machine
                .resolve_import("kernel32.dll", "HeapReAlloc")
                .unwrap();
            let free = machine.resolve_import("kernel32.dll", "HeapFree").unwrap();
            let heap = machine.ext.get::<adv_os::Heaps>().process_heap;
            let old_size = 0x80001;
            let size = 0x9000d;
            let old = moving.then(|| {
                let block = machine
                    .ext
                    .get_mut::<adv_os::Heaps>()
                    .alloc(&mut machine.cpu, &mut machine.sys.vm, heap, old_size, false)
                    .unwrap();
                machine
                    .cpu
                    .write(block, &vec![0x3c; old_size as usize])
                    .unwrap();
                block
            });
            let dirty = machine
                .ext
                .get_mut::<adv_os::Heaps>()
                .alloc(&mut machine.cpu, &mut machine.sys.vm, heap, size, false)
                .unwrap();
            machine
                .cpu
                .write(dirty, &vec![0xa5; size as usize])
                .unwrap();
            let mut code = Asm::new(CODE);
            code.push_imm(dirty)
                .push_imm(0)
                .push_imm(heap)
                .call(free)
                .mov_mem_eax(DATA);
            code.push_imm(size);
            if let Some(old) = old {
                code.push_imm(old);
            }
            code.push_imm(0)
                .push_imm(heap)
                .call(if moving { realloc } else { alloc })
                .mov_mem_eax(DATA + 4);
            execute(&mut machine, code);
            assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 1);
            let new = machine.cpu.read_u32(DATA + 4).unwrap();
            assert_ne!(new, 0);
            let mut bytes = vec![0xff; size as usize];
            machine.cpu.read(new, &mut bytes).unwrap();
            let preserved = if moving { old_size as usize } else { 0 };
            assert!(bytes[..preserved].iter().all(|&b| b == 0x3c));
            assert!(bytes[preserved..].iter().all(|&b| b == 0));
        }
    }
}

#[test]
fn large_global_local_and_heap_allocations_retire_without_stale_aliases() {
    for jit in [false, true] {
        let mut machine = machine(jit);
        let global_alloc = machine
            .resolve_import("kernel32.dll", "GlobalAlloc")
            .unwrap();
        let global_free = machine
            .resolve_import("kernel32.dll", "GlobalFree")
            .unwrap();
        let local_alloc = machine
            .resolve_import("kernel32.dll", "LocalAlloc")
            .unwrap();
        let local_free = machine.resolve_import("kernel32.dll", "LocalFree").unwrap();
        let heap_alloc = machine.resolve_import("kernel32.dll", "HeapAlloc").unwrap();
        let heap_free = machine.resolve_import("kernel32.dll", "HeapFree").unwrap();
        let heap_size = machine.resolve_import("kernel32.dll", "HeapSize").unwrap();
        let heap = machine.ext.get::<adv_os::Heaps>().process_heap;
        let size = 0x90001;
        let mut code = Asm::new(CODE);
        code.push_imm(size)
            .push_imm(0x40)
            .call(global_alloc)
            .mov_mem_eax(DATA);
        code.push_eax().call(global_free).mov_mem_eax(DATA + 4);
        code.push_imm(size + 8)
            .push_imm(0x40)
            .call(local_alloc)
            .mov_mem_eax(DATA + 8);
        code.push_eax().call(local_free).mov_mem_eax(DATA + 12);
        code.push_imm(size + 16)
            .push_imm(8)
            .push_imm(heap)
            .call(heap_alloc)
            .mov_mem_eax(DATA + 16);
        code.push_eax()
            .push_imm(0)
            .push_imm(heap)
            .call(heap_size)
            .mov_mem_eax(DATA + 20);
        code.mov_eax_mem(DATA + 16)
            .mov_eax_ptr_eax()
            .mov_mem_eax(DATA + 32);
        code.mov_eax_mem(DATA + 16)
            .push_eax()
            .push_imm(0)
            .push_imm(heap)
            .call(heap_free)
            .mov_mem_eax(DATA + 24);
        code.mov_eax_mem(DATA + 16)
            .push_eax()
            .push_imm(0)
            .push_imm(heap)
            .call(heap_size)
            .mov_mem_eax(DATA + 28);
        execute(&mut machine, code);
        let first = machine.cpu.read_u32(DATA).unwrap();
        let local = machine.cpu.read_u32(DATA + 8).unwrap();
        let plain = machine.cpu.read_u32(DATA + 16).unwrap();
        assert_ne!(first, 0);
        assert_ne!(local, 0);
        assert_ne!(plain, 0);
        assert_eq!(plain % 8, 0);
        assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), size + 16);
        assert_eq!(machine.cpu.read_u32(DATA + 24).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(DATA + 28).unwrap(), u32::MAX);
        let globals = machine.ext.get::<Globals>();
        assert_eq!(globals.from_pointer(&machine.sys.objects, plain), None);
        assert_eq!(globals.from_allocation(&machine.sys.objects, plain), None);
        assert_eq!(machine.ext.get::<adv_os::Heaps>().owner(plain), None);
        assert_eq!(machine.cpu.read_u32(DATA + 32).unwrap(), 0);
        assert!(!machine.cpu.accessible(plain, 1, Prot::READ));
    }
}
