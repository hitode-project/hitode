//! Guest cdecl calls and thread cleanup against an authored VC80 oracle.
use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const WORKER: u32 = CODE + 3072;
const DATA: u32 = 0x410000;
const OBSERVED: u32 = DATA + 2048;
const THREAD_ID: u32 = DATA + 2052;
const HANDLE: u32 = DATA + 2056;
const LAST_ERROR: u32 = 0x12345678;
const DEFAULT: [u32; 8] = [41, 18467, 6334, 26500, 19169, 15724, 11478, 29358];
const SEEDED: [(u32, [u32; 8]); 5] = [
    (1, DEFAULT),
    (0, [38, 7719, 21238, 2437, 8855, 11797, 8365, 32285]),
    (u32::MAX, [35, 29739, 3374, 11141, 31308, 7870, 5253, 2445]),
    (
        0x80000000,
        [38, 7719, 21238, 2437, 8855, 11797, 8365, 32285],
    ),
    (
        0x6ac5807b,
        [14183, 18271, 2382, 13230, 21235, 3370, 31968, 24412],
    ),
];

fn observe(env: &mut Env<'_>) -> Outcome {
    // The guest has waited for ExitThread. That retired only the worker's
    // random cells; all three main-thread CRT owners still have their state.
    let state = env.ext.get::<CrtState>();
    assert_eq!(state.random_seeds.len(), 3);
    assert!(
        state
            .random_seeds
            .keys()
            .all(|(_, context)| *context == env.context)
    );
    env.cpu.write_u32(OBSERVED, 0xcafe).unwrap();
    Outcome::Return(1)
}

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    registry.add("fixture.dll", "observe", Conv::Cdecl { argc: 0 }, observe);
    for module in MODULES {
        assert_eq!(
            registry.get(module, "rand").unwrap().conv,
            Conv::Cdecl { argc: 0 }
        );
        assert_eq!(
            registry.get(module, "srand").unwrap().conv,
            Conv::Cdecl { argc: 1 }
        );
        assert!(registry.dynamic_available(module, "rand"));
        assert!(registry.dynamic_available(module, "srand"));
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-random-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn push_memory(code: &mut Asm, address: u32) {
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}

fn guest<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let entries: Vec<_> = MODULES
        .iter()
        .map(|module| {
            (
                machine.resolve_import(module, "rand").unwrap(),
                machine.resolve_import(module, "srand").unwrap(),
                machine.resolve_import(module, "_errno").unwrap(),
            )
        })
        .collect();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let create = machine
        .resolve_import("kernel32.dll", "CreateThread")
        .unwrap();
    let wait = machine
        .resolve_import("kernel32.dll", "WaitForSingleObject")
        .unwrap();
    let close = machine
        .resolve_import("kernel32.dll", "CloseHandle")
        .unwrap();
    let exit_thread = machine
        .resolve_import("kernel32.dll", "ExitThread")
        .unwrap();
    let exit_process = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let observer = machine.resolve_import("fixture.dll", "observe").unwrap();
    let mut main = Asm::new(CODE);
    let mut expected = Vec::new();
    let mut offset = 0;
    let record = |code: &mut Asm, value: u32, expected: &mut Vec<(u32, u32)>, offset: &mut u32| {
        code.mov_mem_eax(DATA + *offset);
        expected.push((DATA + *offset, value));
        *offset += 4;
    };
    main.bytes.extend_from_slice(&[0x89, 0xe5]); // Preserve ESP in EBP to verify cdecl balance.
    for &(random, seed, errno) in &entries {
        main.call(errno);
        main.bytes.extend_from_slice(&[0xc7, 0x00]); // mov dword ptr[eax],79.
        main.bytes.extend_from_slice(&79u32.to_le_bytes());
        main.push_imm(LAST_ERROR).call(set_error);
        // Seeding a previous module must not consume this module's default1.
        for value in DEFAULT {
            main.call(random);
            record(&mut main, value, &mut expected, &mut offset);
        }
        for (initial, sequence) in SEEDED {
            main.push_imm(initial).call(seed).add_esp(4);
            for value in sequence {
                main.call(random);
                record(&mut main, value, &mut expected, &mut offset);
            }
        }
        main.call(errno).bytes.extend_from_slice(&[0x8b, 0x00]);
        record(&mut main, 79, &mut expected, &mut offset);
        main.call(get_error);
        record(&mut main, LAST_ERROR, &mut expected, &mut offset);
    }
    main.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    main.mov_mem_eax(DATA + 2000);
    main.bytes.extend_from_slice(&[0x89, 0xe8]); // mov eax,ebp
    main.mov_mem_eax(DATA + 2004);

    let (random, seed, _) = entries[1];
    main.push_imm(0x6ac5807b).call(seed).add_esp(4).call(random);
    record(&mut main, 14183, &mut expected, &mut offset);
    main.push_imm(THREAD_ID)
        .push_imm(0)
        .push_imm(0)
        .push_imm(WORKER)
        .push_imm(0)
        .push_imm(0)
        .call(create)
        .mov_mem_eax(HANDLE);
    main.push_imm(u32::MAX);
    push_memory(&mut main, HANDLE);
    main.call(wait);
    record(&mut main, 0, &mut expected, &mut offset);
    push_memory(&mut main, HANDLE);
    main.call(close);
    record(&mut main, 1, &mut expected, &mut offset);
    main.call(random);
    record(&mut main, 18271, &mut expected, &mut offset);
    for &(_, _, errno) in &entries {
        main.call(errno).bytes.extend_from_slice(&[0x8b, 0x00]);
        record(&mut main, 79, &mut expected, &mut offset);
    }
    main.call(observer).push_imm(0).call(exit_process);
    main.ret();
    assert!(main.bytes.len() < 3072);

    let mut worker = Asm::new(WORKER);
    for (index, &(random, _, _)) in entries.iter().enumerate() {
        worker
            .call(random)
            .mov_mem_eax(DATA + 2100 + index as u32 * 4);
    }
    let (random, seed, _) = entries[1];
    worker
        .push_imm(5)
        .call(seed)
        .add_esp(4)
        .call(random)
        .mov_mem_eax(DATA + 2112);
    worker.push_imm(0).call(exit_thread).ret();
    machine.cpu.write(CODE, &main.bytes).unwrap();
    machine.cpu.write(WORKER, &worker.bytes).unwrap();
    machine
        .spawn("CRT PRNG cdecl and thread isolation", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (address, value) in expected {
        assert_eq!(
            machine.cpu.read_u32(address).unwrap(),
            value,
            "{address:#x}"
        );
    }
    assert_eq!(
        machine.cpu.read_u32(DATA + 2000).unwrap(),
        machine.cpu.read_u32(DATA + 2004).unwrap()
    );
    for index in 0..3 {
        assert_eq!(machine.cpu.read_u32(DATA + 2100 + index * 4).unwrap(), 41);
    }
    assert_eq!(machine.cpu.read_u32(DATA + 2112).unwrap(), 54);
    assert_eq!(machine.cpu.read_u32(OBSERVED).unwrap(), 0xcafe);
    assert!(
        machine.ext.get::<CrtState>().random_seeds.is_empty(),
        "process detach releases its seed map"
    );
}

#[test]
fn vc80_sequences_cdecl_module_thread_isolation_and_detach() {
    guest(JitCpu::new(PARK_ADDR, false).unwrap());
    guest(JitCpu::new(PARK_ADDR, true).unwrap());
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
}
