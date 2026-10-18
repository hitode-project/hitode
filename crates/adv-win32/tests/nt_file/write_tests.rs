//! Ordinary synchronous creation and writes retain the shared VFS owner.
use super::*;

fn on_backends(mut test: impl FnMut(JitCpu), unicorn: impl FnOnce(UnicornCpu)) {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        test(cpu);
    }
    unicorn(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn write_call<B: Backend>(
    m: &mut Machine<B>,
    code: &mut Asm,
    name: &str,
    args: [u32; 9],
    result: u32,
) {
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let write = m.resolve_import("ntdll.dll", name).unwrap();
    code.push_imm(MARKER).call(set);
    for value in args.into_iter().rev() {
        code.push_imm(value);
    }
    code.call(write).mov_mem_eax(result);
    code.call(get).mov_mem_eax(result + 4);
}

fn dispositions<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    let mut code = Asm::new(CODE);
    let cases = [
        (1, true, 0, 1, b"base".as_slice()),
        (1, false, 0xc0000034, 0, b"".as_slice()),
        (2, true, 0xc0000035, 0, b"base".as_slice()),
        (2, false, 0, 2, b"".as_slice()),
        (3, true, 0, 1, b"base".as_slice()),
        (3, false, 0, 2, b"".as_slice()),
        (4, true, 0, 3, b"".as_slice()),
        (4, false, 0xc0000034, 0, b"".as_slice()),
        (5, true, 0, 3, b"".as_slice()),
        (5, false, 0, 2, b"".as_slice()),
    ];
    for (index, (disposition, exists, _, _, _)) in cases.iter().enumerate() {
        let base = DATA + index as u32 * 128;
        let name = format!("disposition-{index}.txt");
        if *exists {
            std::fs::write(root.join("game").join(&name), b"base").unwrap();
        }
        input(
            &mut m,
            base,
            SOURCE + index as u32 * 512,
            &format!(r"\??\C:\Game\{name}"),
        );
        let mut args = arguments(base);
        args[1] = 0x100003; // Specific FILE_READ_DATA/FILE_WRITE_DATA, not generic rights.
        args[6] = 7;
        args[7] = *disposition;
        create(&mut m, &mut code, args, base + 16);
    }
    // Neither creating a missing parent nor truncating without write access
    // may mutate the filesystem through the common opener.
    let missing = DATA + 0x800;
    input(
        &mut m,
        missing,
        SOURCE + 0x3000,
        r"\??\C:\Game\absent\new.txt",
    );
    let mut args = arguments(missing);
    args[1] |= 0x40000000;
    args[7] = 5;
    create(&mut m, &mut code, args, missing + 16);
    let denied = DATA + 0x880;
    input(&mut m, denied, SOURCE + 0x3200, r"\??\C:\Game\present.txt");
    let mut args = arguments(denied);
    args[7] = 4;
    create(&mut m, &mut code, args, denied + 16);
    run(&mut m, code);
    assert_eq!(m.cpu.read_u32(missing + 16).unwrap(), 0xc000003a);
    assert_eq!(m.cpu.read_u32(denied + 16).unwrap(), 0xc0000022);
    assert!(m.ext.get::<Win32>().vfs.lookup(r"C:\Game\absent").is_none());
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let mut code = Asm::new(CODE);
    for (index, (_, existed, status, information, expected)) in cases.into_iter().enumerate() {
        let base = DATA + index as u32 * 128;
        let guest = format!(r"C:\Game\disposition-{index}.txt");
        assert_eq!(m.cpu.read_u32(base + 16).unwrap(), status, "{guest}");
        assert_eq!(m.cpu.read_u32(base + 20).unwrap(), MARKER);
        if status == 0 {
            assert_eq!(m.cpu.read_u32(base + 8).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(base + 12).unwrap(), information);
            let handle = m.cpu.read_u32(base).unwrap();
            assert_eq!(
                m.sys.objects.host::<FileObj>(handle).unwrap().access,
                0x100003
            );
            code.push_imm(handle).call(close);
        } else {
            assert_eq!(m.cpu.read_u32(base).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(base + 8).unwrap(), MARKER);
            assert_eq!(m.cpu.read_u32(base + 12).unwrap(), INFORMATION);
        }
        let selected = m.ext.get::<Win32>().vfs.lookup(&guest);
        if status == 0 || existed {
            assert_eq!(std::fs::read(selected.unwrap()).unwrap(), expected);
        } else {
            assert!(selected.is_none());
        }
        if existed {
            assert_eq!(
                std::fs::read(root.join("game").join(format!("disposition-{index}.txt"))).unwrap(),
                b"base",
                "writes and truncation stay in the overlay"
            );
        }
    }
    run(&mut m, code);
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    assert_eq!(
        std::fs::read(root.join("resources/present.txt")).unwrap(),
        b"replacement!"
    );
}

#[test]
fn native_dispositions_use_copy_up_and_report_open_create_or_overwrite() {
    on_backends(dispositions, dispositions);
}

fn write_positions<B: Backend>(cpu: B) {
    const VIEW: u32 = 0x450000;
    let (mut m, root) = fixture(cpu);
    input(&mut m, DATA, SOURCE, r"\??\C:\Game\present.txt");
    let mut args = arguments(DATA);
    args[1] = 0x100003;
    args[6] = 7;
    let mut code = Asm::new(CODE);
    create(&mut m, &mut code, args, DATA + 16);
    run(&mut m, code);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    let handle = m.cpu.read_u32(DATA).unwrap();
    let alias = m.sys.objects.duplicate(handle).unwrap();
    let selected = m
        .ext
        .get::<Win32>()
        .vfs
        .lookup(r"C:\Game\present.txt")
        .unwrap();
    let backing = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&selected)
        .unwrap();
    let section = m
        .ext
        .get::<Win32>()
        .vfs
        .sections
        .file(backing, 12, true)
        .unwrap();
    let page = section.page(0);
    m.cpu
        .map_file_pages(VIEW, std::slice::from_ref(&page), Prot::RW, false)
        .unwrap();
    let cases = [
        ("ZwWriteFile", handle, None, b"save".as_slice(), 4),
        ("NtWriteFile", alias, Some(8i64), b"XY".as_slice(), 10),
        ("ZwWriteFile", handle, Some(-2), b"!".as_slice(), 11),
        ("NtWriteFile", alias, Some(15), b"Z".as_slice(), 16),
    ];
    let mut code = Asm::new(CODE);
    code.mov_mem_imm(VIEW + 4, 0x54524944); // Dirty neighbor must survive ordinary writes.
    for (index, (name, target, offset, data, _)) in cases.iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        m.cpu.write(base, &[0xcd; 128]).unwrap();
        m.cpu.write(base + 8, data).unwrap();
        let offset_pointer = if let Some(value) = offset {
            m.cpu.write(base + 32, &value.to_le_bytes()).unwrap();
            base + 32
        } else {
            0
        };
        write_call(
            &mut m,
            &mut code,
            name,
            [
                *target,
                0,
                0,
                0,
                base,
                base + 8,
                data.len() as u32,
                offset_pointer,
                0,
            ],
            base + 24,
        );
        query(
            &mut m,
            &mut code,
            [handle, base + 40, base + 48, 8, 14],
            base + 56,
        );
    }
    let read = DATA + 0x600;
    m.cpu.write(read, &[0xcd; 128]).unwrap();
    m.cpu.write(read + 32, &0i64.to_le_bytes()).unwrap();
    read_call(
        &mut m,
        &mut code,
        [alias, 0, 0, 0, read, read + 8, 16, read + 32, 0],
        read + 24,
    );
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    code.push_imm(alias)
        .call(close)
        .push_imm(handle)
        .call(close);
    run(&mut m, code);
    for (index, (_, _, _, data, position)) in cases.into_iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 28).unwrap(), MARKER);
        assert_eq!(m.cpu.read_u32(base).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 4).unwrap(), data.len() as u32);
        assert_eq!(m.cpu.read_u32(base + 48).unwrap(), position);
    }
    assert_eq!(std::fs::read(selected).unwrap(), b"saveacemXY!!\0\0\0Z");
    let mut merged = [0; 16];
    m.cpu.read(read + 8, &mut merged).unwrap();
    assert_eq!(&merged, b"saveDIRTXY!!\0\0\0Z");
    assert_eq!(m.cpu.read_u32(read + 24).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(read + 4).unwrap(), 16);
    assert_eq!(&page.snapshot().unwrap()[..16], &merged);
    assert!(page.dirty());
    assert_eq!(
        std::fs::read(root.join("game/present.txt")).unwrap(),
        b"base"
    );
    assert_eq!(
        std::fs::read(root.join("resources/present.txt")).unwrap(),
        b"replacement!"
    );
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn native_writes_share_offsets_and_publish_committed_bytes_to_dirty_mappings() {
    on_backends(write_positions, write_positions);
}

fn guards<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    let mut code = Asm::new(CODE);
    for (index, access) in [0x100001, 0x100002].into_iter().enumerate() {
        let base = DATA + index as u32 * 128;
        input(
            &mut m,
            base,
            SOURCE + index as u32 * 512,
            r"\??\C:\Game\present.txt",
        );
        let mut args = arguments(base);
        args[1] = access;
        args[6] = 7;
        create(&mut m, &mut code, args, base + 16);
    }
    run(&mut m, code);
    let read_handle = m.cpu.read_u32(DATA).unwrap();
    let write_handle = m.cpu.read_u32(DATA + 128).unwrap();
    assert_eq!(
        m.sys.objects.host::<FileObj>(write_handle).unwrap().access,
        0x100002
    );
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    let failures = [
        ("readonly IOSB", 0xc0000005),
        ("input overrun", 0xc0000005),
        ("offset overrun", 0xc0000005),
        ("negative", 0xc000000d),
        ("end-of-file", 0xc00000bb),
        ("invalid", 0xc0000008),
        ("read-only handle", 0xc0000022),
        ("event", 0xc00000bb),
    ];
    let mut code = Asm::new(CODE);
    for (index, (label, _)) in failures.iter().enumerate() {
        let base = DATA + 0x200 + index as u32 * 128;
        m.cpu.write(base, &[0xcd; 128]).unwrap();
        let mut args = [write_handle, 0, 0, 0, base, base + 8, 2, base + 32, 0];
        m.cpu.write(base + 32, &5i64.to_le_bytes()).unwrap();
        match *label {
            "readonly IOSB" => args[4] = READ_ONLY,
            "input overrun" => args[5] = SOURCE + 0xffff,
            "offset overrun" => args[7] = SOURCE + 0xffff,
            "negative" => m.cpu.write(base + 32, &(-3i64).to_le_bytes()).unwrap(),
            "end-of-file" => m.cpu.write(base + 32, &(-1i64).to_le_bytes()).unwrap(),
            "invalid" => args[0] = 0x13572468,
            "read-only handle" => args[0] = read_handle,
            "event" => args[1] = 1,
            _ => unreachable!(),
        }
        write_call(&mut m, &mut code, "ZwWriteFile", args, base + 24);
        query(
            &mut m,
            &mut code,
            [write_handle, base + 40, base + 48, 8, 14],
            base + 56,
        );
    }
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    for (index, (api, handle)) in [("WriteFile", read_handle), ("ReadFile", write_handle)]
        .into_iter()
        .enumerate()
    {
        let base = DATA + 0x700 + index as u32 * 32;
        m.cpu.write(base, &[0xcd; 32]).unwrap();
        let call = m.resolve_import("kernel32.dll", api).unwrap();
        code.push_imm(0)
            .push_imm(base + 8)
            .push_imm(2)
            .push_imm(base)
            .push_imm(handle)
            .call(call)
            .mov_mem_eax(base + 16)
            .call(get)
            .mov_mem_eax(base + 20);
    }
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    code.push_imm(read_handle)
        .call(close)
        .push_imm(write_handle)
        .call(close);
    run(&mut m, code);
    for (index, (label, status)) in failures.into_iter().enumerate() {
        let base = DATA + 0x200 + index as u32 * 128;
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), status, "{label}");
        assert_eq!(m.cpu.read_u32(base + 28).unwrap(), MARKER);
        assert_eq!(m.cpu.read_u32(base).unwrap(), 0xcdcdcdcd);
        assert_eq!(m.cpu.read_u32(base + 4).unwrap(), 0xcdcdcdcd);
        assert_eq!(m.cpu.read_u32(base + 48).unwrap(), 0, "cursor: {label}");
    }
    for base in [DATA + 0x700, DATA + 0x720] {
        assert_eq!(m.cpu.read_u32(base + 16).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 20).unwrap(), 5);
        assert_eq!(m.cpu.read_u32(base).unwrap(), 0xcdcdcdcd);
    }
    let selected = m
        .ext
        .get::<Win32>()
        .vfs
        .lookup(r"C:\Game\present.txt")
        .unwrap();
    assert_eq!(std::fs::read(selected).unwrap(), b"replacement!");
    assert_eq!(
        std::fs::read(root.join("game/present.txt")).unwrap(),
        b"base"
    );
    assert_eq!(
        std::fs::read(root.join("resources/present.txt")).unwrap(),
        b"replacement!"
    );
    let mut bytes = [0; 32];
    m.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 32]);
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn native_write_preflight_and_specific_rights_prevent_mutation_or_win32_escalation() {
    on_backends(guards, guards);
}
