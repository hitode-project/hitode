use super::*;
use adv_abi::signature;
use adv_os::Heaps;

fn array_case<B: Backend>(cpu: B, module: &str) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-iob",
            "fixture.exe",
            "/nonexistent/crt-iob-data",
        ),
    )
    .unwrap();
    let id = m
        .load(&File(fixture::image(module, &["_iob"])), "data.exe")
        .unwrap();
    let host = adv_os::modules(&m.ext).by_name(module).unwrap();
    let base = adv_os::modules(&m.ext).get(host).base;
    let getter = if signature::lookup(module, "__iob_func").is_some() {
        "__iob_func"
    } else {
        "__p__iob"
    };
    let getter = m.resolve_import(module, getter).unwrap();
    let get_proc = m.resolve_import("kernel32.dll", "GetProcAddress").unwrap();
    let name = fixture::OUTPUT + 128;
    m.cpu.write(name, b"_iob\0").unwrap();
    let mut a = Asm::new(fixture::ENTRY);
    a.mov_eax_mem(fixture::IAT).mov_mem_eax(fixture::OUTPUT);
    a.call(getter).mov_mem_eax(fixture::OUTPUT + 4);
    a.push_imm(name)
        .push_imm(base)
        .call(get_proc)
        .mov_mem_eax(fixture::OUTPUT + 8);
    // Mutating the last FILE object proves that the array is not the old
    // 128-byte heap placeholder or a pointer-sized data declaration.
    a.mov_eax_mem(fixture::IAT);
    a.bytes.extend_from_slice(&[0xc7, 0x80]);
    a.bytes.extend_from_slice(&636u32.to_le_bytes());
    a.bytes.extend_from_slice(&0x12345678u32.to_le_bytes());
    a.call(getter).mov_mem_eax(fixture::OUTPUT + 12);
    a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(fixture::ENTRY, &a.bytes).unwrap();
    m.spawn("CRT FILE array", fixture::ENTRY, &[], 65536)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{module}: {result:?}"
    );
    let pointer = m.cpu.read_u32(fixture::OUTPUT).unwrap();
    for offset in [4, 8, 12] {
        assert_eq!(m.cpu.read_u32(fixture::OUTPUT + offset).unwrap(), pointer);
    }
    assert!(pointer >= base && pointer + 640 <= base + adv_os::modules(&m.ext).get(host).size);
    assert!(m.cpu.accessible(pointer, 640, Prot::RW));
    assert!(!m.cpu.accessible(pointer, 640, Prot::EXEC));
    let input = m.cpu.read_u32(pointer).unwrap();
    assert_ne!(input, 0);
    assert_eq!(m.cpu.read_u32(pointer + 8).unwrap(), input);
    assert_eq!(m.cpu.read_u32(pointer + 12).unwrap(), 0x101);
    assert_eq!(m.cpu.read_u32(pointer + 24).unwrap(), 4096);
    assert!(m.ext.get::<Heaps>().owner(&m.cpu, input).is_some());
    assert_eq!(m.cpu.read_u32(input + 4092).unwrap(), 0);
    for i in 0..3 {
        assert_eq!(m.cpu.read_u32(pointer + i * 32 + 16).unwrap(), u32::MAX - 1);
        assert_eq!(m.cpu.read_u32(pointer + i * 32 + 4).unwrap(), 0);
        if i != 0 {
            assert_eq!(m.cpu.read_u32(pointer + i * 32).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(pointer + i * 32 + 12).unwrap(), 2);
        }
    }
    for offset in (96..636).step_by(4) {
        assert_eq!(m.cpu.read_u32(pointer + offset).unwrap(), 0);
    }
    assert_eq!(m.cpu.read_u32(pointer + 636).unwrap(), 0x12345678);
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
    let _ = id;
}

// spec: CRT-LIB-25, PERS-EXPORT-6
#[test]
fn legacy_file_arrays_have_real_data_iats_layout_and_getter_identity_on_all_backends() {
    let modules: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| s.name == "_iob")
        .map(|s| s.module)
        .collect();
    assert_eq!(modules.len(), 10);
    for module in modules {
        for jit in [false, true] {
            array_case(JitCpu::new(PARK_ADDR, jit).unwrap(), module);
        }
        array_case(UnicornCpu::new(PARK_ADDR).unwrap(), module);
    }
}

fn independent<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-iob",
            "fixture.exe",
            "/nonexistent/crt-iob-data",
        ),
    )
    .unwrap();
    m.load(&File(fixture::image("msvcr80.dll", &["_iob"])), "data.exe")
        .unwrap();
    assert!(adv_os::modules(&m.ext).by_name("msvcr90.dll").is_none());
    let first = m.resolve_import("msvcr80.dll", "__iob_func").unwrap();
    let second = m.resolve_import("msvcr90.dll", "__iob_func").unwrap();
    let mut a = Asm::new(fixture::ENTRY);
    a.call(first).mov_mem_eax(fixture::OUTPUT);
    a.bytes.extend_from_slice(&[0xc7, 0x80]);
    a.bytes.extend_from_slice(&628u32.to_le_bytes());
    a.bytes.extend_from_slice(&0x12345678u32.to_le_bytes());
    a.call(second).mov_mem_eax(fixture::OUTPUT + 4);
    a.call(first).mov_mem_eax(fixture::OUTPUT + 8);
    a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(fixture::ENTRY, &a.bytes).unwrap();
    m.spawn("independent CRT arrays", fixture::ENTRY, &[], 65536)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let p = m.cpu.read_u32(fixture::OUTPUT).unwrap();
    let q = m.cpu.read_u32(fixture::OUTPUT + 4).unwrap();
    assert_ne!(p, q);
    assert_eq!(m.cpu.read_u32(fixture::OUTPUT + 8).unwrap(), p);
    assert_eq!(m.cpu.read_u32(p + 628).unwrap(), 0x12345678);
    assert_eq!(m.cpu.read_u32(q + 628).unwrap(), 0);
    assert_ne!(m.cpu.read_u32(p).unwrap(), m.cpu.read_u32(q).unwrap());
    assert!(adv_os::modules(&m.ext).by_name("msvcr90.dll").is_some());
}

// spec: CRT-LIB-25, PERS-EXPORT-6
#[test]
fn legacy_arrays_activate_only_their_own_runtime_and_preserve_independent_guest_writes() {
    for jit in [false, true] {
        independent(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    independent(UnicornCpu::new(PARK_ADDR).unwrap());
}
