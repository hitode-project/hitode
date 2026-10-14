//! Authored legacy RTL calls against the native DOS/NT path contract.
use adv_abi::{Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::Heaps;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const SOURCE: u32 = 0x430000;
const MARKER: u32 = 0x12345678;

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    fixture_in(
        cpu,
        "/nonexistent/nt-path".into(),
        "/nonexistent/nt-path-data".into(),
    )
}

fn fixture_in<B: Backend>(
    cpu: B,
    game: std::path::PathBuf,
    data: std::path::PathBuf,
) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(&mut m, Win32Config::new(game, "fixture.exe", data)).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.map(SOURCE, 0x20000, Prot::RW).unwrap();
    m
}

fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("NT paths", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

fn descriptor<B: Backend>(m: &Machine<B>, address: u32) -> (u16, u16, u32) {
    let sizes = m.cpu.read_u32(address).unwrap();
    (
        sizes as u16,
        (sizes >> 16) as u16,
        m.cpu.read_u32(address + 4).unwrap(),
    )
}

fn converted_names_and_free<B: Backend>(cpu: B) {
    let mut m = fixture(cpu);
    let convert = m
        .resolve_import("ntdll.dll", "RtlDosPathNameToNtPathName_U")
        .unwrap();
    let free = m
        .resolve_import("ntdll.dll", "RtlFreeUnicodeString")
        .unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    // Six cases come from the authored x86 native oracle; the relative native
    // cwd is replaced with this personality's independently configured cwd.
    let cases = [
        (r"C:\oracle\..\file.txt", r"\??\C:\file.txt", None),
        (
            r"relative\file.txt",
            r"\??\C:\Game\relative\file.txt",
            Some(r"relative\file.txt"),
        ),
        (
            r"\\server\share\dir\file",
            r"\??\UNC\server\share\dir\file",
            None,
        ),
        (r"\\?\C:\oracle\..\file. ", r"\??\C:\oracle\..\file. ", None),
        (r"\\.\NUL", r"\??\NUL", None),
        (r"\??\C:\file.txt", r"\??\C:\file.txt", None),
        (r"..\file.txt", r"\??\C:\file.txt", None),
        (
            r"C:relative\file.txt",
            r"\??\C:\Game\relative\file.txt",
            Some(r"relative\file.txt"),
        ),
        (r"\rooted\file.txt", r"\??\C:\rooted\file.txt", None),
        (
            "字\\🌻.txt",
            "\\??\\C:\\Game\\字\\🌻.txt",
            Some("字\\🌻.txt"),
        ),
        (r"C:\oracle\", r"\??\C:\oracle\", None),
    ];
    let mut code = Asm::new(CODE);
    for (index, (input, _, _)) in cases.iter().enumerate() {
        let source = SOURCE + index as u32 * 512;
        let bytes: Vec<u8> = input
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        m.cpu.write(source, &bytes).unwrap();
        let output = DATA + index as u32 * 64;
        m.cpu.write(output, &[0xcd; 64]).unwrap();
        code.push_imm(MARKER).call(set_error);
        code.push_imm(output + 12)
            .push_imm(output + 8)
            .push_imm(output)
            .push_imm(source)
            .call(convert)
            .mov_mem_eax(output + 32);
        code.call(get_error).mov_mem_eax(output + 36);
    }
    run(&mut m, code);
    let heap = m.ext.get::<Heaps>().process_heap;
    let mut buffers = Vec::new();
    for (index, (_, expected, relative)) in cases.iter().enumerate() {
        let output = DATA + index as u32 * 64;
        assert_eq!(m.cpu.read_u32(output + 32).unwrap(), 1);
        assert_eq!(m.cpu.read_u32(output + 36).unwrap(), MARKER);
        let (length, maximum, buffer) = descriptor(&m, output);
        let units: Vec<u16> = expected.encode_utf16().collect();
        assert_eq!(length as usize, units.len() * 2);
        assert_eq!(maximum, length + 2);
        assert_eq!(
            m.ext.get_mut::<Heaps>().size(&m.cpu, heap, buffer),
            Some(u32::from(maximum))
        );
        let mut bytes = vec![0; maximum as usize];
        m.cpu.read(buffer, &mut bytes).unwrap();
        assert_eq!(
            bytes,
            units
                .iter()
                .copied()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        let file_part = m.cpu.read_u32(output + 8).unwrap();
        let expected_part = if expected.ends_with('\\') {
            0
        } else {
            let prefix = &expected[..expected.rfind('\\').unwrap() + 1];
            buffer + prefix.encode_utf16().count() as u32 * 2
        };
        assert_eq!(
            file_part, expected_part,
            "basename points into the same allocation"
        );
        let relative_descriptor = descriptor(&m, output + 12);
        assert_eq!(
            relative_descriptor,
            relative.map_or((0, 0, 0), |relative| {
                let bytes = relative.encode_utf16().count() as u16 * 2;
                (bytes, bytes, buffer + u32::from(length - bytes))
            })
        );
        assert_eq!(
            m.cpu.read_u32(output + 20).unwrap(),
            0,
            "no host directory handle"
        );
        assert_eq!(m.cpu.read_u32(output + 24).unwrap(), 0, "no host CurDirRef");
        buffers.push(buffer);
    }
    let mut release = Asm::new(CODE);
    for index in 0..cases.len() {
        let output = DATA + index as u32 * 64;
        release
            .push_imm(MARKER)
            .call(set_error)
            .push_imm(output)
            .call(free);
        release.call(get_error).mov_mem_eax(output + 36);
    }
    run(&mut m, release);
    for (index, buffer) in buffers.into_iter().enumerate() {
        assert_eq!(descriptor(&m, DATA + index as u32 * 64), (0, 0, 0));
        assert_eq!(
            m.ext.get_mut::<Heaps>().size(&m.cpu, heap, buffer),
            None,
            "the ordinary RTL free owns this allocation"
        );
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 64 + 36).unwrap(),
            MARKER
        );
    }
}

#[test]
fn dos_nt_names_and_aliased_views_share_the_rtl_allocator_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        converted_names_and_free(cpu);
    }
    converted_names_and_free(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn failed_conversions<B: Backend>(cpu: B) {
    let mut m = fixture(cpu);
    let convert = m
        .resolve_import("ntdll.dll", "RtlDosPathNameToNtPathName_U")
        .unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    m.cpu.write(SOURCE, &[0, 0]).unwrap();
    let long = format!("C:\\{}", "x".repeat(32761));
    m.cpu
        .write(
            SOURCE + 0x1000,
            &long
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let valid = SOURCE + 0x800;
    m.cpu
        .write(valid, &[b'C', 0, b':', 0, b'\\', 0, b'a', 0, 0, 0])
        .unwrap();
    let mut code = Asm::new(CODE);
    let cases = [
        (SOURCE, DATA, DATA + 8, DATA + 12),
        (0, DATA, DATA + 8, DATA + 12),
        (0x600000, DATA, DATA + 8, DATA + 12),
        (SOURCE + 0x1000, DATA, DATA + 8, DATA + 12),
        (valid, 0, DATA + 8, DATA + 12),
        (valid, READ_ONLY, DATA + 8, DATA + 12),
        (valid, DATA, READ_ONLY, DATA + 12),
        (valid, DATA, DATA + 8, READ_ONLY),
        (valid, DATA, DATA + 8, u32::MAX - 7),
    ];
    m.cpu.write(DATA, &[0xcd; 32]).unwrap();
    for (index, (source, out, part, relative)) in cases.into_iter().enumerate() {
        code.push_imm(MARKER)
            .call(set_error)
            .push_imm(relative)
            .push_imm(part)
            .push_imm(out)
            .push_imm(source)
            .call(convert)
            .mov_mem_eax(DATA + 0x100 + index as u32 * 8);
        code.call(get_error)
            .mov_mem_eax(DATA + 0x104 + index as u32 * 8);
    }
    run(&mut m, code);
    for index in 0..cases.len() {
        assert_eq!(m.cpu.read_u32(DATA + 0x100 + index as u32 * 8).unwrap(), 0);
        assert_eq!(
            m.cpu.read_u32(DATA + 0x104 + index as u32 * 8).unwrap(),
            MARKER
        );
    }
    let mut bytes = [0; 32];
    m.cpu.read(DATA, &mut bytes).unwrap();
    assert_eq!(bytes, [0xcd; 32]);
    m.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 32]);
}

#[test]
fn failed_conversions_preserve_all_outputs_and_last_error_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        failed_conversions(cpu);
    }
    failed_conversions(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn only_the_called_legacy_export_is_projected_and_implemented() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert_eq!(
        registry
            .get("ntdll.dll", "RtlDosPathNameToNtPathName_U")
            .unwrap()
            .conv,
        Conv::Stdcall { argc: 4 }
    );
    assert_eq!(
        registry.level("ntdll.dll", "RtlDosPathNameToNtPathName_U"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("ntdll.dll", "RtlDosPathNameToNtPathName_U"));
    assert!(!registry.dynamic_available("ntdll.dll", "RtlDosPathNameToNtPathName_U_WithStatus"));
    assert!(registry.signature_conflicts().is_empty());
}

fn device_names<B: Backend>(cpu: B) {
    // A directory-qualified NUL needs an existing parent directory.
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/nt-path-tests");
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::create_dir_all(root.join("data/c")).unwrap();
    let mut m = fixture_in(cpu, root.join("game"), root.join("data"));
    let convert = m
        .resolve_import("ntdll.dll", "RtlDosPathNameToNtPathName_U")
        .unwrap();
    // From the x86 native oracle. A device keeps the caller's spelling and has
    // neither a file part nor a relative view; near misses stay relative files.
    let devices = [
        ("NUL", "NUL"),
        ("nul", "nul"),
        ("NUL:", "NUL"),
        ("NUL ", "NUL"),
        ("NUL.", "NUL"),
        ("nul.", "nul"),
        ("NUL :", "NUL"),
        (r".\NUL", "NUL"),
        (r"..\NUL", "NUL"),
        ("C:NUL", "NUL"),
        (r"\NUL", "NUL"),
        ("/NUL", "NUL"),
        (r"C:\NUL", "NUL"),
        ("CON", "CON"),
        ("CON:", "CON"),
        ("AUX", "AUX"),
        ("PRN", "PRN"),
        ("COM1", "COM1"),
        ("COM9", "COM9"),
        ("LPT1", "LPT1"),
    ];
    let files = ["NUL.txt", "NULx", "COM0", "COM10", "LPT0", "CLOCK$"];
    let inputs: Vec<&str> = devices
        .iter()
        .map(|(input, _)| *input)
        .chain(files)
        .collect();
    let mut code = Asm::new(CODE);
    for (index, input) in inputs.iter().enumerate() {
        let source = SOURCE + index as u32 * 64;
        let bytes: Vec<u8> = input
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        m.cpu.write(source, &bytes).unwrap();
        let output = DATA + index as u32 * 32;
        m.cpu.write(output, &[0xcd; 32]).unwrap();
        code.push_imm(output + 12)
            .push_imm(output + 8)
            .push_imm(output)
            .push_imm(source)
            .call(convert)
            .mov_mem_eax(output + 28);
    }
    run(&mut m, code);
    for (index, input) in inputs.iter().enumerate() {
        let output = DATA + index as u32 * 32;
        assert_eq!(m.cpu.read_u32(output + 28).unwrap() & 0xff, 1, "{input}");
        let (length, _, buffer) = descriptor(&m, output);
        let mut bytes = vec![0; length as usize];
        m.cpu.read(buffer, &mut bytes).unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let name = String::from_utf16(&units).unwrap();
        let file_part = m.cpu.read_u32(output + 8).unwrap();
        let relative = descriptor(&m, output + 12);
        if let Some((_, device)) = devices.get(index) {
            assert_eq!(name, format!(r"\??\{device}"), "{input}");
            assert_eq!(file_part, 0, "{input}");
            assert_eq!(relative, (0, 0, 0), "{input}");
        } else {
            assert_eq!(name, format!(r"\??\C:\Game\{input}"), "{input}");
            let prefix = r"\??\C:\Game\".encode_utf16().count() as u32 * 2;
            assert_eq!(file_part, buffer + prefix, "{input}");
            let bytes = input.len() as u16 * 2;
            assert_eq!(relative, (bytes, bytes, buffer + prefix), "{input}");
        }
    }
}

#[test]
fn dos_device_names_convert_without_file_parts_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        device_names(cpu);
    }
    device_names(UnicornCpu::new(PARK_ADDR).unwrap());
}
