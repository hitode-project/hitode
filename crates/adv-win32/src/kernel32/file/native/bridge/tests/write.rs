use super::*;

const WRITER: &str = "authored-write-provider";

#[derive(Default)]
struct Writer {
    information: u32,
    mode: u32,
    dispositions: Vec<u32>,
    attempts: Vec<(u64, u32)>,
    bytes: Vec<u8>,
}

fn configure(env: &mut Env<'_>) -> Outcome {
    let (mode, information) = (env.arg(0), env.arg(1));
    let writer = env.ext.get_mut::<Writer>();
    writer.mode = mode;
    writer.information = information;
    if mode == 5 {
        env.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    }
    Outcome::Return(0)
}

fn open(env: &mut Env<'_>) -> Outcome {
    observe(env);
    let (output, object, iosb, disposition) = (env.arg(0), env.arg(2), env.arg(3), env.arg(7));
    assert_eq!(env.arg(1) & 0xc000_0000, 0xc000_0000);
    assert_eq!(env.arg(5), 0x80);
    assert_eq!(env.arg(6), 5);
    assert_ne!(env.arg(8) & 0x20, 0);
    let name = env.cpu.read_u32(object + 8).unwrap();
    let length = env.cpu.read_u16(name).unwrap();
    let pointer = env.cpu.read_u32(name + 4).unwrap();
    let units: Vec<_> = (0..u32::from(length) / 2)
        .map(|index| env.cpu.read_u16(pointer + index * 2).unwrap())
        .collect();
    let path = String::from_utf16(&units).unwrap();
    assert_eq!(path, r"\??\C:\Game\shadow.dat");
    let (provider, writer) = env.ext.get2_mut::<Provider, Writer>();
    provider.paths.push(path);
    provider.allocations.push(object);
    writer.dispositions.push(disposition);
    if writer.mode == 4 {
        return Outcome::Return(0xc000_0022);
    }
    assert!(!provider.opened);
    provider.opened = true;
    provider.cursor = 0;
    if matches!(disposition, 2 | 4 | 5) {
        writer.bytes.clear();
    }
    provider.size = Some(writer.bytes.len() as u64);
    env.cpu.write_u32(output, HANDLE).unwrap();
    env.cpu.write_u32(iosb, 0).unwrap();
    env.cpu.write_u32(iosb + 4, writer.information).unwrap();
    Outcome::Return(0)
}

fn write(env: &mut Env<'_>) -> Outcome {
    observe(env);
    assert_eq!(env.arg(0), HANDLE);
    let (iosb, source, length, offset) = (env.arg(4), env.arg(5), env.arg(6), env.arg(7));
    assert_eq!(offset, 0, "use the provider's shared cursor");
    assert!(length <= 4);
    let (provider, writer) = env.ext.get2_mut::<Provider, Writer>();
    assert!(provider.opened);
    assert_ne!(
        writer.mode, 5,
        "readonly written_out must fail before NtWriteFile"
    );
    writer.attempts.push((provider.cursor, length));
    env.cpu.write_u32(iosb + 4, 0).unwrap();
    if writer.mode == 1 {
        return Outcome::Return(0xc000_0022);
    }
    if writer.mode == 3 {
        env.cpu.write_u32(iosb + 4, length + 1).unwrap();
        return Outcome::Return(0);
    }
    // A short successful transfer makes the bridge consume the provider's count.
    let count = length.min(2);
    let position = provider.cursor as usize;
    writer
        .bytes
        .resize(writer.bytes.len().max(position + count as usize), 0);
    env.cpu
        .read(
            source,
            &mut writer.bytes[position..position + count as usize],
        )
        .unwrap();
    provider.cursor += u64::from(count);
    provider.size = Some(writer.bytes.len() as u64);
    env.cpu.write_u32(iosb, 0).unwrap();
    env.cpu.write_u32(iosb + 4, count).unwrap();
    if writer.mode == 2 {
        env.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    }
    Outcome::Return(0)
}

fn read(env: &mut Env<'_>) -> Outcome {
    observe(env);
    assert_eq!(env.arg(0), HANDLE);
    let (iosb, output, capacity, offset) = (env.arg(4), env.arg(5), env.arg(6), env.arg(7));
    assert_eq!(offset, 0);
    let (provider, writer) = env.ext.get2_mut::<Provider, Writer>();
    assert!(provider.opened);
    provider.reads.push((provider.cursor, capacity));
    let position = provider.cursor as usize;
    let count = (writer.bytes.len().saturating_sub(position)).min(capacity as usize);
    env.cpu
        .write(output, &writer.bytes[position..position + count])
        .unwrap();
    provider.cursor += count as u64;
    env.cpu.write_u32(iosb, 0).unwrap();
    env.cpu.write_u32(iosb + 4, count as u32).unwrap();
    Outcome::Return(0)
}

fn fixture(name: &str) -> (Machine<JitCpu>, PathBuf) {
    let (mut machine, root) = super::fixture(name);
    {
        let registry = machine.sys.thunks.resolver_mut::<Registry>().unwrap();
        for (name, argc, handler) in [
            ("open", 11, open as adv_abi::Handler),
            ("write", 9, write as adv_abi::Handler),
            ("read", 9, read as adv_abi::Handler),
            ("configure", 2, configure as adv_abi::Handler),
        ] {
            registry.add(WRITER, name, Conv::Stdcall { argc }, handler);
        }
    }
    for (nt, name) in [
        ("NtCreateFile", "open"),
        ("NtWriteFile", "write"),
        ("NtReadFile", "read"),
    ] {
        let target = machine.resolve_import(WRITER, name).unwrap();
        let entry = native_entry(&mut machine, nt);
        let provider = machine.ext.get_mut::<Provider>();
        provider.hooks.retain(|&(old, _)| old != entry);
        provider.hooks.push((entry, target));
    }
    machine.ext.insert(Writer::default());
    machine.cpu.write(DATA + 0x200, b"save").unwrap();
    machine.cpu.write_u32(DATA + 0x140, SENTINEL).unwrap();
    (machine, root)
}

#[test]
fn a_w_create_dispositions_and_opaque_writes_keep_provider_state_and_errors() {
    for wide in [false, true] {
        let (mut machine, root) = fixture(if wide { "write-wide" } else { "write-ansi" });
        let create = api(
            &mut machine,
            if wide { "CreateFileW" } else { "CreateFileA" },
        );
        let write = api(&mut machine, "WriteFile");
        let read = api(&mut machine, "ReadFile");
        let seek = api(&mut machine, "SetFilePointerEx");
        let close = api(&mut machine, "CloseHandle");
        let errors = (
            api(&mut machine, "SetLastError"),
            api(&mut machine, "GetLastError"),
        );
        let install = machine.resolve_import(PROVIDER, "install").unwrap();
        let restore = machine.resolve_import(PROVIDER, "restore").unwrap();
        let configure = machine.resolve_import(WRITER, "configure").unwrap();
        let native_write = native_entry(&mut machine, "NtWriteFile");
        let path = DATA + if wide { 0x500 } else { 0x400 };
        let mut code = Asm::new(CODE);
        code.bytes.extend([0x89, 0xe0]);
        code.mov_mem_eax(OUT + 120).call(install);
        let cases = [
            (1, 2, 2, 0),
            (2, 5, 3, 183),
            (3, 1, 1, 0),
            (4, 3, 1, 183),
            (5, 4, 3, 0),
            (2, 5, 2, 0),
            (4, 3, 2, 0),
        ];
        let mut expected = Vec::new();
        for (index, &(disposition, _, information, error)) in cases.iter().enumerate() {
            let offset = 0x100 + index as u32 * 16;
            code.push_imm(information).push_imm(0).call(configure);
            call(
                &mut code,
                create,
                &[path, 0xc000_0000, 5, 0, disposition, 0x80, 0],
                OUT + offset,
                errors,
            );
            call(&mut code, close, &[HANDLE], OUT + offset + 8, errors);
            expected.extend([
                (offset, HANDLE),
                (offset + 4, error),
                (offset + 8, 1),
                (offset + 12, SENTINEL),
            ]);
        }
        code.push_imm(2).push_imm(0).call(configure);
        call(
            &mut code,
            create,
            &[path, 0xc000_0000, 5, 0, 2, 0x80, 0],
            OUT,
            errors,
        );
        call(
            &mut code,
            write,
            &[HANDLE, DATA + 0x200, 4, DATA + 0x100, 0],
            OUT + 8,
            errors,
        );
        // Direct NT I/O and Win32 I/O must advance the same provider cursor.
        call(
            &mut code,
            native_write,
            &[HANDLE, 0, 0, 0, DATA + 0x108, DATA + 0x202, 2, 0, 0],
            OUT + 16,
            errors,
        );
        call(
            &mut code,
            seek,
            &[HANDLE, 0, 0, DATA + 0x110, 1],
            OUT + 24,
            errors,
        );
        call(
            &mut code,
            seek,
            &[HANDLE, 0, 0, DATA + 0x150, 0],
            OUT + 32,
            errors,
        );
        call(
            &mut code,
            read,
            &[HANDLE, DATA + 0x220, 4, DATA + 0x118, 0],
            OUT + 40,
            errors,
        );
        code.push_imm(2).push_imm(5).call(configure);
        call(
            &mut code,
            write,
            &[HANDLE, DATA + 0x200, 2, DATA + 0x140, 0],
            OUT + 48,
            errors,
        );
        code.call(restore).push_imm(2).push_imm(1).call(configure);
        call(
            &mut code,
            write,
            &[HANDLE, DATA + 0x200, 2, DATA + 0x120, 0],
            OUT + 56,
            errors,
        );
        code.push_imm(2).push_imm(2).call(configure);
        call(
            &mut code,
            write,
            &[HANDLE, DATA + 0x200, 2, DATA + 0x128, 0],
            OUT + 64,
            errors,
        );
        code.call(restore).push_imm(2).push_imm(3).call(configure);
        call(
            &mut code,
            write,
            &[HANDLE, DATA + 0x200, 2, DATA + 0x130, 0],
            OUT + 72,
            errors,
        );
        call(&mut code, close, &[HANDLE], OUT + 80, errors);
        code.push_imm(2).push_imm(4).call(configure);
        // Native failure is authoritative even when the original VFS has the file.
        call(
            &mut code,
            create,
            &[path, 0xc000_0000, 5, 0, 2, 0x80, 0],
            OUT + 88,
            errors,
        );
        finish(&mut machine, code);
        expected.extend([
            (0, HANDLE),
            (4, 0),
            (8, 1),
            (12, SENTINEL),
            (16, 0),
            (20, SENTINEL),
            (24, 1),
            (28, 0),
            (32, 1),
            (36, 0),
            (40, 1),
            (44, SENTINEL),
            (48, 0),
            (52, 998),
            (56, 0),
            (60, 5),
            (64, 0),
            (68, 998),
            (72, 0),
            (76, 87),
            (80, 1),
            (84, SENTINEL),
            (88, u32::MAX),
            (92, 5),
        ]);
        outputs(&machine, &expected);
        assert_eq!(machine.cpu.read_u32(DATA + 0x100).unwrap(), 2);
        assert_eq!(machine.cpu.read_u32(DATA + 0x10c).unwrap(), 2);
        assert_eq!(read_u64(&machine.cpu, DATA + 0x110), 4);
        assert_eq!(machine.cpu.read_u32(DATA + 0x118).unwrap(), 4);
        assert_eq!(machine.cpu.read_u32(DATA + 0x140).unwrap(), SENTINEL);
        for offset in [0x120, 0x128, 0x130] {
            assert_eq!(machine.cpu.read_u32(DATA + offset).unwrap(), 0);
        }
        let mut read_back = [0; 4];
        machine.cpu.read(DATA + 0x220, &mut read_back).unwrap();
        assert_eq!(&read_back, b"save");
        let writer = machine.ext.get::<Writer>();
        let mut dispositions: Vec<_> = cases.iter().map(|case| case.1).collect();
        dispositions.extend([5, 5]);
        assert_eq!(writer.dispositions, dispositions);
        assert_eq!(writer.attempts, [(0, 4), (2, 2), (4, 2), (4, 2), (6, 2)]);
        assert_eq!(writer.bytes, b"savesa");
        let provider = machine.ext.get::<Provider>();
        assert_eq!(provider.cursor, 6);
        assert_eq!(provider.reads, [(0, 4)]);
        assert_eq!(provider.closes, cases.len() + 1);
        assert!(!provider.opened);
        assert_eq!(
            std::fs::read(root.join("game/shadow.dat")).unwrap(),
            b"original"
        );
        assert!(!root.join("data/overlay/shadow.dat").exists());
    }
}
