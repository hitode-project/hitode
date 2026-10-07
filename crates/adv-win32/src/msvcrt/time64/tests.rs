use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const INPUT: u32 = DATA + 0x2000;
const ERRNO: u32 = DATA + 0x1800;

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        for (name, argc) in [
            ("_time64", 1),
            ("_gmtime64", 1),
            ("_localtime64", 1),
            ("_difftime64", 4),
        ] {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc }
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    registry.add("fixture.dll", "copy_tm", Conv::Cdecl { argc: 2 }, |env| {
        let input = GuestPtr::<[i32; 9]>::new(env.arg(0));
        let output = GuestPtr::<[i32; 9]>::new(env.arg(1));
        match input
            .read(env.cpu)
            .and_then(|value| output.write(env.cpu, value))
        {
            Ok(()) => Outcome::Return(0),
            Err(error) => memory_failure(error),
        }
    });
    registry.add("fixture.dll", "cleanup", Conv::Cdecl { argc: 0 }, |env| {
        let heap = env.ext.get::<Heaps>().process_heap;
        let owned: Vec<_> = env
            .ext
            .get::<CrtState>()
            .tm_buffers
            .values()
            .copied()
            .collect();
        assert_eq!(owned.len(), 3);
        let other = allocate(env, TM_BYTES, true);
        assert_ne!(other, 0);
        env.ext
            .get_mut::<CrtState>()
            .tm_buffers
            .insert(("msvcr80.dll".into(), env.context + 1), other);
        super::super::detach_thread(env).unwrap();
        for address in owned {
            assert_eq!(env.ext.get_mut::<Heaps>().size(heap, address), None);
        }
        assert_eq!(env.ext.get_mut::<Heaps>().size(heap, other), Some(TM_BYTES));
        assert_eq!(env.ext.get::<CrtState>().tm_buffers.len(), 1);
        super::super::detach_thread(env).unwrap();
        super::super::detach_process(env).unwrap();
        super::super::detach_process(env).unwrap();
        assert!(env.ext.get::<CrtState>().tm_buffers.is_empty());
        assert_eq!(env.ext.get_mut::<Heaps>().size(heap, other), None);
        Outcome::Return(0)
    });
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-time64-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    m.cpu.map(DATA + 16384, 4096, Prot::READ).unwrap();
    m
}
fn call(code: &mut Asm, api: u32, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(api);
    if !args.is_empty() {
        code.add_esp((args.len() * 4) as u8);
    }
}
fn quad(code: &mut Asm, output: u32) {
    code.mov_mem_eax(output);
    code.bytes.extend_from_slice(&[0x89, 0x15]); // mov [absolute],edx
    code.bytes.extend_from_slice(&(output + 4).to_le_bytes());
}
fn fstp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0xdd, 0x1d]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}
fn errno(code: &mut Asm, api: u32) {
    code.call(api).mov_mem_eax(ERRNO);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
}
fn finish<B: Backend>(m: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("time64 authored guest", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn u64_at<B: Backend>(m: &Machine<B>, pointer: u32) -> u64 {
    let mut bytes = [0; 8];
    m.cpu.read(pointer, &mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}
fn snapshot<B: Backend>(m: &Machine<B>, pointer: u32) -> [i32; 9] {
    let mut bytes = [0; 36];
    m.cpu.read(pointer, &mut bytes).unwrap();
    std::array::from_fn(|i| i32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
}

fn clock_calendar<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let target = 0x1_0000_0005u64;
    m.sys
        .clock
        .advance_to(m.sys.clock.epoch_us + (target - 1_577_836_800) * 1_000_000);
    let times = [0i64, 951_782_400, MAX_CALENDAR_UTC];
    for (i, value) in times.into_iter().enumerate() {
        m.cpu
            .write(INPUT + i as u32 * 8, &value.to_le_bytes())
            .unwrap();
    }
    let copy = m.resolve_import("fixture.dll", "copy_tm").unwrap();
    let mut code = Asm::new(CODE);
    let mut calendar_rows = Vec::new();
    let mut module_pointers = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        let base = DATA + module_index as u32 * 512;
        errno(&mut code, m.resolve_import(module, "_errno").unwrap());
        let filetime = m
            .resolve_import("kernel32.dll", "GetSystemTimeAsFileTime")
            .unwrap();
        code.push_imm(base).call(filetime);
        let time = m.resolve_import(module, "_time64").unwrap();
        call(&mut code, time, &[base + 16]);
        quad(&mut code, base + 8);
        call(&mut code, time, &[0]);
        quad(&mut code, base + 24);
        for (row_index, (name, input, expected)) in [
            ("_gmtime64", INPUT, [0, 0, 0, 1, 0, 70, 4, 0, 0]),
            ("_localtime64", INPUT, [0, 0, 9, 1, 0, 70, 4, 0, 0]),
            ("_gmtime64", INPUT + 8, [0, 0, 0, 29, 1, 100, 2, 59, 0]),
            (
                "_gmtime64",
                INPUT + 16,
                [59, 59, 23, 31, 11, 1100, 3, 364, 0],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let row = base + 64 + row_index as u32 * 48;
            call(&mut code, m.resolve_import(module, name).unwrap(), &[input]);
            code.mov_mem_eax(row)
                .push_imm(row + 4)
                .push_eax()
                .call(copy)
                .add_esp(8);
            calendar_rows.push((row, expected));
            if row_index == 0 {
                module_pointers.push(row);
            }
        }
        let diff = m.resolve_import(module, "_difftime64").unwrap();
        call(&mut code, diff, &[1, 0x200000, 0, 0x200000]);
        fstp(&mut code, base + 320);
        call(&mut code, diff, &[40, 0, 100, 0]);
        fstp(&mut code, base + 328);
        code.mov_eax_mem(ERRNO)
            .mov_eax_ptr_eax()
            .mov_mem_eax(base + 336);
    }
    code.call(m.resolve_import("fixture.dll", "cleanup").unwrap());
    let result = finish(&mut m, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, _) in MODULES.into_iter().enumerate() {
        let base = DATA + index as u32 * 512;
        let ft_seconds = (u64_at(&m, base) - UNIX_EPOCH_FT) / 10_000_000;
        let now = u64_at(&m, base + 8);
        assert_eq!(now, u64_at(&m, base + 16));
        assert!((target..=target + 1).contains(&now));
        assert!((ft_seconds..=ft_seconds + 1).contains(&now));
        assert!((now..=now + 1).contains(&u64_at(&m, base + 24)));
        assert_eq!(u64_at(&m, base + 320), 1.0f64.to_bits());
        assert_eq!(u64_at(&m, base + 328), (-60.0f64).to_bits());
        assert_eq!(m.cpu.read_u32(base + 336).unwrap(), 79);
        let pointer = m.cpu.read_u32(base + 64).unwrap();
        for row in 1..4 {
            assert_eq!(m.cpu.read_u32(base + 64 + row * 48).unwrap(), pointer);
        }
    }
    for (row, expected) in calendar_rows {
        assert_eq!(snapshot(&m, row + 4), expected);
    }
    let pointers: Vec<_> = module_pointers
        .into_iter()
        .map(|p| m.cpu.read_u32(p).unwrap())
        .collect();
    assert!(pointers[0] != pointers[1] && pointers[1] != pointers[2] && pointers[0] != pointers[2]);
    assert_eq!(m.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
fn wall_clock_edx_eax_calendar_aliases_shared_tm_and_detach_use_one_owner() {
    clock_calendar(UnicornCpu::new(PARK_ADDR).unwrap());
    clock_calendar(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn errors<B: Backend>(mut cpu: impl FnMut() -> B) {
    let mut m = machine(cpu());
    m.cpu.write(INPUT, &0i64.to_le_bytes()).unwrap();
    m.cpu.write(INPUT + 8, &(-1i64).to_le_bytes()).unwrap();
    m.cpu
        .write(INPUT + 16, &(MAX_CALENDAR_UTC + 1).to_le_bytes())
        .unwrap();
    let errno_api = m.resolve_import("msvcr80.dll", "_errno").unwrap();
    let gmtime = m.resolve_import("msvcr80.dll", "_gmtime64").unwrap();
    let handler_address = CODE + 4096;
    let mut handler = Asm::new(handler_address);
    handler
        .mov_eax_mem(ERRNO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 64);
    handler
        .mov_eax_mem(DATA)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 68);
    handler.bytes.extend_from_slice(&[0xff, 0x05]);
    handler.bytes.extend_from_slice(&(DATA + 72).to_le_bytes());
    handler.ret();
    m.cpu.write(handler_address, &handler.bytes).unwrap();
    let mut code = Asm::new(CODE);
    call(
        &mut code,
        m.resolve_import("msvcr80.dll", "_set_invalid_parameter_handler")
            .unwrap(),
        &[handler_address],
    );
    call(&mut code, gmtime, &[INPUT]);
    code.mov_mem_eax(DATA);
    errno(&mut code, errno_api);
    call(&mut code, gmtime, &[INPUT + 8]);
    code.mov_mem_eax(DATA + 4);
    code.mov_eax_mem(ERRNO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 8);
    errno(&mut code, errno_api);
    call(&mut code, gmtime, &[0]);
    code.mov_mem_eax(DATA + 12);
    errno(&mut code, errno_api);
    call(&mut code, gmtime, &[INPUT + 16]);
    code.mov_mem_eax(DATA + 16);
    code.mov_eax_mem(ERRNO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 20);
    errno(&mut code, errno_api);
    call(
        &mut code,
        m.resolve_import("msvcr80.dll", "_difftime64").unwrap(),
        &[u32::MAX, u32::MAX, 40, 0],
    );
    fstp(&mut code, DATA + 24);
    code.mov_eax_mem(ERRNO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 32);
    assert!(matches!(finish(&mut m, code), RunResult::Exited(0)));
    for p in [DATA + 4, DATA + 12, DATA + 16] {
        assert_eq!(m.cpu.read_u32(p).unwrap(), 0);
    }
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 22);
    assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 40);
    assert_eq!(u64_at(&m, DATA + 24), 0);
    assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 22);
    assert_eq!(m.cpu.read_u32(DATA + 64).unwrap(), 22);
    assert_eq!(m.cpu.read_u32(DATA + 68).unwrap(), u32::MAX);
    assert_eq!(m.cpu.read_u32(DATA + 72).unwrap(), 1);
    assert_eq!(snapshot(&m, m.cpu.read_u32(DATA).unwrap()), [-1; 9]);
    assert!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api == "msvcr80.dll!_gmtime64")
    );
    for output in [false, true] {
        let mut m = machine(cpu());
        let mut code = Asm::new(CODE);
        if output {
            m.cpu.write(DATA + 16380, &[0x5a; 4]).unwrap();
            call(
                &mut code,
                m.resolve_import("msvcr80.dll", "_time64").unwrap(),
                &[DATA + 16380],
            );
        } else {
            m.cpu.write(DATA + 16380, &[0x5a; 4]).unwrap();
            call(
                &mut code,
                m.resolve_import("msvcr80.dll", "_gmtime64").unwrap(),
                &[DATA + 16384 + 4092],
            );
        }
        assert!(matches!(finish(&mut m, code), RunResult::Failed(_)));
        assert_eq!(m.cpu.read_u32(DATA + 16380).unwrap(), 0x5a5a5a5a);
        assert!(
            m.ext
                .try_get::<CrtState>()
                .is_none_or(|s| s.tm_buffers.is_empty())
        );
    }
}

#[test]
fn invalid_calendar_callback_errno_range_and_memory_guards_are_distinct() {
    errors(|| UnicornCpu::new(PARK_ADDR).unwrap());
    errors(|| JitCpu::new(PARK_ADDR, false).unwrap());
}
