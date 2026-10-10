//! The null device behind `\??\NUL`, compared with the native x86 oracle.
use super::*;

const UNTOUCHED: u32 = 0xa5a5_a5a5;

fn on_backends(mut test: impl FnMut(JitCpu), unicorn: impl FnOnce(UnicornCpu)) {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        test(cpu);
    }
    unicorn(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn opens<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    // (name, access, share, disposition, options, status)
    let cases = [
        (r"\??\NUL", 0x80100080, 1, 1, 0x60, 0),
        (r"\??\nul", 0x80100080, 0, 1, 0x60, 0),
        (r"\??\NUL", 0xc0100080, 0, 0, 0x60, 0),
        (r"\??\NUL", 0xc0100080, 0, 2, 0x60, 0),
        (r"\??\NUL", 0xc0100080, 0, 3, 0x60, 0),
        (r"\??\NUL", 0xc0100080, 0, 4, 0x60, 0),
        (r"\??\NUL", 0xc0100080, 0, 5, 0x60, 0),
        (r"\??\NUL", 0x80100080, 7, 1, 0x20, 0),
        (r"\??\NUL\x", 0x80100080, 7, 1, 0x60, 0),
        (r"\Device\Null", 0x80100080, 7, 1, 0x60, 0),
        // Native reports c0000034 for these other \?? names. Which names exist
        // there depends on the machine, so they stay explicitly unavailable.
        (r"\??\NUL:x", 0x80100080, 7, 1, 0x60, 0xc00000bb),
        (r"\??\NUL.txt", 0x80100080, 7, 1, 0x60, 0xc00000bb),
        (r"\??\NUL", 0x00010000, 7, 1, 0x60, 0xc000000d),
    ];
    let mut code = Asm::new(CODE);
    for (index, (name, access, share, disposition, options, status)) in cases.iter().enumerate() {
        let base = DATA + index as u32 * 128;
        input(&mut m, base, SOURCE + index as u32 * 512, name);
        let mut args = arguments(base);
        (args[1], args[6], args[7], args[8]) = (*access, *share, *disposition, *options);
        create(&mut m, &mut code, args, base + 16);
        if *status == 0 {
            code.mov_eax_mem(base)
                .push_eax()
                .call(close)
                .mov_mem_eax(base + 72);
        }
    }
    run(&mut m, code);
    for (index, (name, _, _, disposition, _, status)) in cases.into_iter().enumerate() {
        let base = DATA + index as u32 * 128;
        let label = format!("{name} disposition {disposition} case {index}");
        assert_eq!(m.cpu.read_u32(base + 16).unwrap(), status, "{label}");
        assert_eq!(m.cpu.read_u32(base + 20).unwrap(), MARKER, "{label}");
        if status == 0 {
            // Every disposition reports FILE_SUPERSEDED (zero) information.
            assert_ne!(m.cpu.read_u32(base).unwrap(), 0, "{label}");
            assert_eq!(m.cpu.read_u32(base + 8).unwrap(), 0, "{label}");
            assert_eq!(m.cpu.read_u32(base + 12).unwrap(), 0, "{label}");
            assert_eq!(m.cpu.read_u32(base + 72).unwrap(), 1, "{label}");
        } else {
            assert_eq!(m.cpu.read_u32(base + 8).unwrap(), MARKER, "{label}");
            assert_eq!(m.cpu.read_u32(base + 12).unwrap(), INFORMATION, "{label}");
        }
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    assert!(!root.join("game/NUL").exists() && !root.join("data").join("c/NUL").exists());
}

#[test]
fn null_device_names_open_with_every_disposition_like_native() {
    on_backends(opens, opens);
}

/// Guest memory for one call: its result, its last error and a scratch block.
struct Slots(u32);
impl Slots {
    fn take(&mut self, size: u32) -> u32 {
        let address = self.0;
        self.0 += size;
        address
    }
}

/// Calls `target(handle, rest...)` with the handle stored at `handle`, keeping
/// EAX at `result` and the last error at `result + 4`.
fn handle_call<B: Backend>(
    m: &mut Machine<B>,
    code: &mut Asm,
    target: u32,
    handle: u32,
    rest: &[u32],
    result: u32,
) {
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    m.cpu.write_u32(result, MARKER).unwrap();
    code.push_imm(MARKER).call(set);
    for value in rest.iter().rev() {
        code.push_imm(*value);
    }
    code.mov_eax_mem(handle)
        .push_eax()
        .call(target)
        .mov_mem_eax(result);
    code.call(get).mov_mem_eax(result + 4);
}

/// One operation's observable results: status/return, last error, IOSB or
/// count output, and the first dwords of its output buffer.
struct Observed {
    result: u32,
    iosb: u32,
    buffer: u32,
}

fn operations<B: Backend>(cpu: B) {
    let (mut m, _root) = fixture(cpu);
    let names = [
        "NtReadFile",
        "NtWriteFile",
        "NtQueryInformationFile",
        "NtQueryVolumeInformationFile",
        "NtSetInformationFile",
        "NtFlushBuffersFile",
    ];
    let nt: Vec<u32> = names
        .iter()
        .map(|name| m.resolve_import("ntdll.dll", name).unwrap())
        .collect();
    let k32 = |m: &mut Machine<B>, name| m.resolve_import("kernel32.dll", name).unwrap();
    let (read_file, write_file) = (k32(&mut m, "ReadFile"), k32(&mut m, "WriteFile"));
    let (file_type, file_size) = (k32(&mut m, "GetFileType"), k32(&mut m, "GetFileSize"));
    let pointer = k32(&mut m, "SetFilePointer");
    let information = k32(&mut m, "GetFileInformationByHandle");
    let (flush, end) = (k32(&mut m, "FlushFileBuffers"), k32(&mut m, "SetEndOfFile"));
    let close = k32(&mut m, "CloseHandle");
    let mut code = Asm::new(CODE);
    let mut slots = Slots(DATA + 0x400);
    let text = slots.take(16);
    m.cpu.write(text, b"hello\0\0\0").unwrap();
    let zero = slots.take(8);
    m.cpu.write(zero, &[0; 8]).unwrap();
    let hundred = slots.take(8);
    m.cpu.write(hundred, &100u64.to_le_bytes()).unwrap();
    let mut observed = Vec::new();
    for (index, access) in [0x80100080u32, 0xc0100080].into_iter().enumerate() {
        let base = DATA + index as u32 * 128;
        input(&mut m, base, SOURCE + index as u32 * 512, r"\??\NUL");
        let mut args = arguments(base);
        (args[1], args[6]) = (access, 7);
        create(&mut m, &mut code, args, base + 16);
        let mut run_one =
            |m: &mut Machine<B>, code: &mut Asm, target, rest: &dyn Fn(u32, u32) -> Vec<u32>| {
                let result = slots.take(8);
                let iosb = slots.take(8);
                let buffer = slots.take(64);
                m.cpu.write(iosb, &[0xa5; 8]).unwrap();
                m.cpu.write(buffer, &[0xa5; 64]).unwrap();
                handle_call(m, code, target, base, &rest(iosb, buffer), result);
                Observed {
                    result,
                    iosb,
                    buffer,
                }
            };
        let mut handle = Vec::new();
        handle.push(run_one(&mut m, &mut code, file_type, &|_, _| vec![]));
        handle.push(run_one(&mut m, &mut code, read_file, &|n, b| {
            vec![b, 16, n, 0]
        }));
        handle.push(run_one(&mut m, &mut code, read_file, &|n, b| {
            vec![b, 0, n, 0]
        }));
        handle.push(run_one(&mut m, &mut code, write_file, &|n, _| {
            vec![text, 5, n, 0]
        }));
        handle.push(run_one(&mut m, &mut code, nt[0], &|i, b| {
            vec![0, 0, 0, i, b, 16, zero, 0]
        }));
        handle.push(run_one(&mut m, &mut code, nt[1], &|i, _| {
            vec![0, 0, 0, i, text, 3, zero, 0]
        }));
        // Access and mode information come from the I/O manager for every file
        // and are not modeled here yet.
        for class in [4, 5, 14] {
            handle.push(run_one(&mut m, &mut code, nt[2], &|i, b| {
                vec![i, b, 64, class]
            }));
        }
        for class in [1, 4] {
            handle.push(run_one(&mut m, &mut code, nt[3], &|i, b| {
                vec![i, b, 64, class]
            }));
        }
        handle.push(run_one(&mut m, &mut code, nt[4], &|i, _| {
            vec![i, hundred, 8, 14]
        }));
        handle.push(run_one(&mut m, &mut code, nt[5], &|i, _| vec![i]));
        handle.push(run_one(&mut m, &mut code, file_size, &|high, _| vec![high]));
        handle.push(run_one(&mut m, &mut code, pointer, &|_, _| vec![10, 0, 0]));
        handle.push(run_one(&mut m, &mut code, information, &|_, b| vec![b]));
        handle.push(run_one(&mut m, &mut code, flush, &|_, _| vec![]));
        handle.push(run_one(&mut m, &mut code, end, &|_, _| vec![]));
        handle.push(run_one(&mut m, &mut code, close, &|_, _| vec![]));
        observed.push(handle);
    }
    run(&mut m, code);
    let read = |m: &Machine<B>, address: u32| m.cpu.read_u32(address).unwrap();
    for (index, handle) in observed.iter().enumerate() {
        let writable = index == 1;
        let label = if writable { "read/write" } else { "read-only" };
        assert_eq!(read(&m, DATA + index as u32 * 128 + 16), 0, "{label} open");
        // (return, last error, IOSB/count dwords, buffer dwords)
        let denied = |status| if writable { status } else { 0xc0000022 };
        let error = |code| if writable { code } else { 5 };
        let expected: Vec<(u32, Option<u32>, [u32; 2], Vec<u32>)> = vec![
            (2, Some(MARKER), [UNTOUCHED; 2], vec![]),
            (1, Some(MARKER), [0, UNTOUCHED], vec![UNTOUCHED]),
            (1, Some(MARKER), [0, UNTOUCHED], vec![]),
            if writable {
                (1, Some(MARKER), [5, UNTOUCHED], vec![])
            } else {
                (0, Some(5), [0, UNTOUCHED], vec![])
            },
            (0xc0000011, None, [0xc0000011, 0], vec![UNTOUCHED]),
            if writable {
                (0, None, [0, 3], vec![])
            } else {
                (0xc0000022, None, [UNTOUCHED; 2], vec![])
            },
            (0xc0000003, None, [UNTOUCHED; 2], vec![UNTOUCHED]),
            (0, None, [0, 24], vec![0, 0, 0, 0, 1, 0, UNTOUCHED]),
            (0, None, [0, 8], vec![0, 0, UNTOUCHED]),
            (0xc0000010, None, [UNTOUCHED; 2], vec![UNTOUCHED]),
            (0, None, [0, 8], vec![0x15, 0x100, UNTOUCHED]),
            (0, None, [0, 0], vec![]),
            (denied(0xc0000010), None, [UNTOUCHED; 2], vec![]),
            (0, Some(MARKER), [0, UNTOUCHED], vec![]),
            (10, Some(MARKER), [UNTOUCHED; 2], vec![]),
            (0, Some(1), [UNTOUCHED; 2], vec![UNTOUCHED]),
            (0, Some(error(1)), [UNTOUCHED; 2], vec![]),
            (0, Some(error(1)), [UNTOUCHED; 2], vec![]),
            (1, Some(MARKER), [UNTOUCHED; 2], vec![]),
        ];
        assert_eq!(handle.len(), expected.len());
        for (step, (observed, (result, last_error, iosb, buffer))) in
            handle.iter().zip(expected).enumerate()
        {
            let label = format!("{label} step {step}");
            assert_eq!(read(&m, observed.result), result, "{label} result");
            if let Some(last_error) = last_error {
                assert_eq!(read(&m, observed.result + 4), last_error, "{label} error");
            }
            let status = [read(&m, observed.iosb), read(&m, observed.iosb + 4)];
            assert_eq!(status, iosb, "{label} IOSB/count");
            let data: Vec<u32> = (0..buffer.len() as u32)
                .map(|word| read(&m, observed.buffer + word * 4))
                .collect();
            assert_eq!(data, buffer, "{label} buffer");
        }
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn null_device_reads_end_writes_discard_and_queries_match_native() {
    on_backends(operations, operations);
}

fn win32_opens<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    let create = m.resolve_import("kernel32.dll", "CreateFileA").unwrap();
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    // (name, disposition, opened, last error). Windows 7 truncates the null
    // device while later kernel32 releases reject it, so that stays unavailable.
    let mut cases = Vec::new();
    for name in ["NUL", "nul", r"C:\Game\NUL", r"\\.\NUL", "NUL:"] {
        for disposition in [3, 1, 2, 4] {
            cases.push((name, disposition, true, 0));
        }
        cases.push((name, 5, false, 50));
    }
    cases.push(("NUL.txt", 3, false, 2));
    cases.push(("CON", 3, false, 50));
    let mut code = Asm::new(CODE);
    for (index, (name, disposition, _, _)) in cases.iter().enumerate() {
        let source = SOURCE + index as u32 * 64;
        let mut bytes = name.as_bytes().to_vec();
        bytes.push(0);
        m.cpu.write(source, &bytes).unwrap();
        let result = DATA + index as u32 * 16;
        code.push_imm(MARKER).call(set);
        for value in [0xc0000000, 0, 0, *disposition, 0x80, 0].iter().rev() {
            code.push_imm(*value);
        }
        code.push_imm(source).call(create).mov_mem_eax(result);
        code.call(get).mov_mem_eax(result + 4);
        code.mov_eax_mem(result).push_eax().call(close);
    }
    run(&mut m, code);
    for (index, (name, disposition, opened, error)) in cases.into_iter().enumerate() {
        let result = DATA + index as u32 * 16;
        let label = format!("{name} disposition {disposition}");
        let handle = m.cpu.read_u32(result).unwrap();
        assert_eq!(handle != u32::MAX, opened, "{label}");
        assert_eq!(m.cpu.read_u32(result + 4).unwrap(), error, "{label}");
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    assert!(!root.join("game/NUL").exists() && !root.join("data").join("c/NUL").exists());
}

#[test]
fn win32_null_device_names_open_the_null_device_like_native() {
    on_backends(win32_opens, win32_opens);
}
