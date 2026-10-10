use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Prot, Reg},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use std::path::PathBuf;
use std::rc::Rc;

#[test]
fn optical_volume_file_copy_and_mapping_share_read_only_media() {
    let (mut machine, root) = fixture("optical");
    let image = root.join("media.iso");
    std::fs::write(
        &image,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../adv-os/tests/fixtures/optical.iso"
        )),
    )
    .unwrap();
    machine
        .ext
        .get_mut::<Win32>()
        .vfs
        .mount_iso('D', &image)
        .unwrap();
    machine.cpu.write(0x410000, b"D:\\\0").unwrap(); // privacy: allow authored guest optical path
    args(&mut machine, &[0x410000]);
    assert_eq!(returned(get_drive_type(&mut env(&mut machine))), 5);
    assert_eq!(drive_letters(&env(&mut machine)), vec!['C', 'D']);
    assert_eq!(
        returned(get_logical_drive_strings(
            &mut env(&mut machine),
            0,
            0,
            false
        )),
        9
    );
    assert_eq!(
        returned(get_logical_drive_strings(
            &mut env(&mut machine),
            9,
            0x410020,
            false
        )),
        8
    );
    let mut roots = [0; 9];
    machine.cpu.read(0x410020, &mut roots).unwrap();
    assert_eq!(&roots, b"C:\\\0D:\\\0\0");
    args(
        &mut machine,
        &[
            0x410000, 0x410100, 64, 0x410200, 0x410204, 0x410208, 0x410180, 32,
        ],
    );
    assert_eq!(
        returned(get_volume_information(&mut env(&mut machine), false)),
        1
    );
    assert_eq!(
        machine.cpu.read_cstr(0x410100, 64).unwrap(),
        b"OPTICAL_TEST"
    );
    assert_eq!(machine.cpu.read_cstr(0x410180, 32).unwrap(), b"CDFS");
    assert_eq!(machine.cpu.read_u32(0x410208).unwrap(), 0x80005);
    assert_eq!(machine.cpu.read_u32(0x410204).unwrap(), 110);
    // Short output fails before changing any of the caller's other outputs.
    machine.cpu.write_u32(0x410200, 0xfeedbeef).unwrap();
    args(&mut machine, &[0x410000, 0x410100, 1, 0x410200, 0, 0, 0, 0]);
    assert_eq!(
        returned(get_volume_information(&mut env(&mut machine), false)),
        0
    );
    assert_eq!(machine.cpu.read_u32(0x410200).unwrap(), 0xfeedbeef);
    let file = open_with_share(
        &mut env(&mut machine),
        r"D:\readme.TXT", // privacy: allow authored guest optical path
        0x80000000,
        OPEN_EXISTING,
        7,
    );
    assert_ne!(file, INVALID_HANDLE_VALUE);
    args(&mut machine, &[file, 0x410300, 32, 0x410280, 0]);
    assert_eq!(returned(read_file(&mut env(&mut machine))), 1);
    assert_eq!(machine.cpu.read_u32(0x410280).unwrap(), 11);
    assert_eq!(
        read_contents(&env(&mut machine), r"D:\ReadMe.txt").unwrap(), // privacy: allow authored guest optical path
        b"hello world"
    );
    assert_eq!(
        open(
            &mut env(&mut machine),
            r"D:\ReadMe.txt", // privacy: allow authored guest optical path
            GENERIC_WRITE,
            OPEN_EXISTING
        ),
        INVALID_HANDLE_VALUE
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 19);
    for device in [r"\\.\D:", r"\\?\D:"] {
        assert_eq!(
            open(&mut env(&mut machine), device, 0, OPEN_EXISTING),
            INVALID_HANDLE_VALUE
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 50);
    }
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            r"D:\ReadMe.txt".into(), // privacy: allow authored guest optical path
            r"C:\Game\copy.txt".into(),
            true,
            false
        )),
        1
    );
    assert_eq!(
        read_contents(&env(&mut machine), r"C:\Game\copy.txt").unwrap(),
        b"hello world"
    );
    let section = returned(super::super::file_mapping::create_section(
        &mut env(&mut machine),
        file,
        0,
        2,
        0,
        None,
    ));
    assert_ne!(section, 0);
    let view = returned(super::super::file_mapping::map_section(
        &mut env(&mut machine),
        section,
        4,
        0,
        0,
        0,
    ));
    assert_ne!(view, 0);
    machine.sys.objects.close(file);
    machine.sys.objects.close(section);
    let mut bytes = [0; 11];
    machine.cpu.read(view, &mut bytes).unwrap();
    assert_eq!(&bytes, b"hello world");
    assert_eq!(
        returned(super::super::file_mapping::unmap_section(
            &mut env(&mut machine),
            view
        )),
        1
    );
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

fn fixture(name: &str) -> (Machine<JitCpu>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/file-mutation-contract-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
    )
    .unwrap();
    machine.spawn("files", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
    machine.cpu.set_reg(Reg::Esp, 0x410800);
    (machine, root)
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("unexpected deferred file operation"),
    }
}
fn args(machine: &mut Machine<JitCpu>, values: &[u32]) {
    for (i, value) in values.iter().enumerate() {
        machine
            .cpu
            .write_u32(0x410804 + i as u32 * 4, *value)
            .unwrap();
    }
}
fn mapping(
    machine: &mut Machine<JitCpu>,
    path: &str,
    writable: bool,
) -> Rc<adv_os::vfs::FileSection> {
    let path = machine.ext.get::<Win32>().vfs.lookup(path).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(writable)
        .open(path)
        .unwrap();
    let length = file.metadata().unwrap().len();
    let section = machine
        .ext
        .get::<Win32>()
        .vfs
        .sections
        .file(file, length, writable)
        .unwrap();
    machine
        .cpu
        .map_file_pages(0x420000, &[section.page(0)], Prot::RW, false)
        .unwrap();
    machine.cpu.read_u8(0x420000).unwrap(); // Materialize the shared page before mutation.
    section
}
fn profile(machine: &mut Machine<JitCpu>, path: &str, value: &str) -> u32 {
    for (address, text) in [
        (0x410100, "s"),
        (0x410110, "k"),
        (0x410120, value),
        (0x410200, path),
    ] {
        machine.cpu.write(address, text.as_bytes()).unwrap();
        machine
            .cpu
            .write(address + text.len() as u32, &[0])
            .unwrap();
    }
    args(machine, &[0x410100, 0x410110, 0x410120, 0x410200]);
    returned(write_private_profile_string_a(&mut env(machine)))
}

#[test]
fn copy_rejects_mapped_target_before_copy_up_or_mutation() {
    let (mut machine, root) = fixture("copy-mapped");
    std::fs::write(root.join("game/source.bin"), [0x22; 8192]).unwrap();
    std::fs::write(root.join("game/target.bin"), [0x11; 4096]).unwrap();
    let target = mapping(&mut machine, "target.bin", false);
    for length in [2048, 4096, 8192] {
        std::fs::write(root.join("game/source.bin"), vec![0x22; length]).unwrap();
        assert_eq!(
            returned(copy_file(
                &mut env(&mut machine),
                "source.bin".into(),
                "target.bin".into(),
                false,
                false
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 1224);
    }
    assert_eq!(machine.cpu.read_u8(0x420000).unwrap(), 0x11);
    assert_eq!(target.page(0).snapshot().unwrap()[0], 0x11);
    assert_eq!(
        std::fs::read(root.join("game/target.bin")).unwrap(),
        [0x11; 4096]
    );
    assert!(!root.join("data/overlay/target.bin").exists());
    // A section owner without a live view still forbids CopyFile replacement.
    machine.cpu.unmap(0x420000, 4096).unwrap();
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            "source.bin".into(),
            "target.bin".into(),
            false,
            false
        )),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 1224);
    drop(target);
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            "source.bin".into(),
            "target.bin".into(),
            false,
            false
        )),
        1
    );
    assert_eq!(
        std::fs::read(root.join("data/overlay/target.bin")).unwrap(),
        [0x22; 8192]
    );
    assert_eq!(
        std::fs::read(root.join("game/target.bin")).unwrap(),
        [0x11; 4096]
    );
}

#[test]
fn profile_updates_hot_mapping_and_old_file_handles_without_touching_original() {
    let (mut machine, root) = fixture("profile-mapped");
    std::fs::write(root.join("game/settings.ini"), b"[s]\r\nk=old\r\n").unwrap();
    let handle = open(
        &mut env(&mut machine),
        "settings.ini",
        0x80000000,
        OPEN_EXISTING,
    );
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    let section = mapping(&mut machine, "settings.ini", false);
    assert_eq!(profile(&mut machine, "C:\\game\\settings.ini", "new"), 1);
    let mut bytes = [0; 12];
    machine.cpu.read(0x420000, &mut bytes).unwrap();
    assert_eq!(&bytes, b"[s]\r\nk=new\r\n");
    let mut old = [0; 12];
    machine
        .sys
        .objects
        .host_mut::<FileObj>(handle)
        .unwrap()
        .file
        .read_exact(&mut old)
        .unwrap();
    assert_eq!(old, bytes);
    assert_eq!(section.page(0).snapshot().unwrap()[7], b'n');
    assert_eq!(
        std::fs::read(root.join("game/settings.ini")).unwrap(),
        b"[s]\r\nk=old\r\n"
    );
    assert_eq!(
        profile(&mut machine, "C:\\game\\settings.ini", "long-value"),
        1
    );
    let expected = b"[s]\r\nk=long-value\r\n";
    let mut growth = [0; 19];
    machine.cpu.read(0x420000, &mut growth).unwrap();
    assert_eq!(&growth, expected);
    let file = &mut machine
        .sys
        .objects
        .host_mut::<FileObj>(handle)
        .unwrap()
        .file;
    assert_eq!(
        file.stream_position().unwrap(),
        12,
        "copy-up preserves an older independent cursor"
    );
    file.seek(SeekFrom::Start(0)).unwrap();
    file.read_exact(&mut growth).unwrap();
    assert_eq!(&growth, expected);
    assert_eq!(profile(&mut machine, "C:\\game\\settings.ini", "x"), 0);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 1224);
    assert_eq!(
        std::fs::read(root.join("data/overlay/settings.ini")).unwrap(),
        expected
    );
    machine.cpu.unmap(0x420000, 4096).unwrap();
    drop(section);
    assert_eq!(profile(&mut machine, "C:\\game\\settings.ini", "x"), 1);
    assert_eq!(
        std::fs::read(root.join("data/overlay/settings.ini")).unwrap(),
        b"[s]\r\nk=x\r\n"
    );
}

#[test]
fn copy_reads_unflushed_shared_source_in_bounded_chunks_and_retargets_old_target_handle() {
    let (mut machine, root) = fixture("copy-source");
    let mut bytes = (0..131123).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    std::fs::write(root.join("game/source.bin"), &bytes).unwrap();
    std::fs::write(root.join("game/target.bin"), [0x11; 4096]).unwrap();
    let handle = open(
        &mut env(&mut machine),
        "target.bin",
        0x80000000,
        OPEN_EXISTING,
    );
    machine
        .sys
        .objects
        .host_mut::<FileObj>(handle)
        .unwrap()
        .file
        .seek(SeekFrom::Start(17))
        .unwrap();
    let source = mapping(&mut machine, "source.bin", false);
    source.page(0).write(1, &[0xaa, 0xbb]).unwrap();
    source.page(16).write(5, &[0xcc, 0xdd]).unwrap();
    bytes[1..3].copy_from_slice(&[0xaa, 0xbb]);
    bytes[65541..65543].copy_from_slice(&[0xcc, 0xdd]);
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            "source.bin".into(),
            "target.bin".into(),
            false,
            false
        )),
        1
    );
    assert_eq!(
        std::fs::read(root.join("data/overlay/target.bin")).unwrap(),
        bytes
    );
    let file = &mut machine
        .sys
        .objects
        .host_mut::<FileObj>(handle)
        .unwrap()
        .file;
    assert_eq!(file.stream_position().unwrap(), 17);
    let mut actual = [0; 13];
    file.read_exact(&mut actual).unwrap();
    assert_eq!(actual, bytes[17..30]);
    assert_eq!(std::fs::read(root.join("game/source.bin")).unwrap()[1], 1);
    assert_eq!(
        std::fs::read(root.join("game/target.bin")).unwrap(),
        [0x11; 4096]
    );
    // Aliases and early failures must not copy-up/truncate the source or target.
    std::fs::hard_link(root.join("game/source.bin"), root.join("game/alias.bin")).unwrap();
    for target in ["source.bin", "SOURCE.bin", "alias.bin"] {
        assert_eq!(
            returned(copy_file(
                &mut env(&mut machine),
                "source.bin".into(),
                target.into(),
                false,
                false
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 32);
    }
    assert!(!root.join("data/overlay/source.bin").exists());
    assert!(!root.join("data/overlay/alias.bin").exists());
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            "missing.bin".into(),
            "target.bin".into(),
            false,
            false
        )),
        0
    );
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            "source.bin".into(),
            "target.bin".into(),
            true,
            false
        )),
        0
    );
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            "source.bin".into(),
            "missing/target.bin".into(),
            false,
            false
        )),
        0
    );
    assert!(!root.join("data/overlay/missing").exists());
    assert_eq!(
        std::fs::read(root.join("data/overlay/target.bin")).unwrap(),
        bytes
    );
    // Retire unflushed read-only test bytes without intentionally persisting them.
    source.page(0).clear_dirty();
    source.page(16).clear_dirty();
}

#[test]
fn failed_profile_style_write_publishes_only_actual_prefix_and_closes_temporary_handle() {
    struct Short<'a> {
        file: &'a mut adv_os::vfs::FileStream,
        committed: usize,
    }
    impl Write for Short<'_> {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            if self.committed == 4 {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            let written = self.file.write(&input[..input.len().min(2)])?;
            self.committed += written;
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }
    let (mut machine, root) = fixture("partial-write");
    std::fs::write(root.join("game/target.ini"), b"abcdefghijkl").unwrap();
    let section = mapping(&mut machine, "target.ini", false);
    let before = machine.sys.objects.hosts::<FileObj>().count();
    let result = replace_contents(
        &mut env(&mut machine),
        "target.ini",
        12,
        false,
        |file, content| {
            content
                .unwrap()
                .write_to(&mut Short { file, committed: 0 }, 0, b"0123456789AB")
        },
    );
    assert_eq!(result, Err(err::ACCESS_DENIED));
    assert_eq!(machine.sys.objects.hosts::<FileObj>().count(), before);
    assert_eq!(&section.page(0).snapshot().unwrap()[..12], b"0123efghijkl");
    assert_eq!(
        std::fs::read(root.join("data/overlay/target.ini")).unwrap(),
        b"0123efghijkl"
    );
    assert_eq!(
        std::fs::read(root.join("game/target.ini")).unwrap(),
        b"abcdefghijkl"
    );
}

#[test]
fn datafile_shrink_rejection_keeps_raw_mapping_length_identity_and_base_bytes() {
    let (mut machine, root) = fixture("datafile-target");
    let mut bytes = vec![0; 8192];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
    bytes[128..132].copy_from_slice(b"PE\0\0");
    bytes[132..134].copy_from_slice(&0x14cu16.to_le_bytes());
    bytes[134..136].copy_from_slice(&1u16.to_le_bytes());
    bytes[148..150].copy_from_slice(&224u16.to_le_bytes());
    bytes[152..154].copy_from_slice(&0x10bu16.to_le_bytes());
    bytes[208..212].copy_from_slice(&0x20000u32.to_le_bytes());
    bytes[212..216].copy_from_slice(&512u32.to_le_bytes());
    bytes[388..392].copy_from_slice(&0x1000u32.to_le_bytes());
    bytes[392..396].copy_from_slice(&4096u32.to_le_bytes());
    bytes[396..400].copy_from_slice(&512u32.to_le_bytes());
    bytes[510..512].copy_from_slice(b"\r\n");
    bytes[512..519].copy_from_slice(b"[s]\r\nk=");
    bytes[519..8190].fill(b'a');
    bytes[8190..].copy_from_slice(b"\r\n");
    std::fs::write(root.join("game/raw.dll"), &bytes).unwrap();
    std::fs::write(root.join("game/short.bin"), [0x22; 12]).unwrap();
    let file = std::fs::File::open(root.join("game/raw.dll")).unwrap();
    let identity = adv_os::vfs::file_identity(&file).unwrap();
    let section = machine
        .ext
        .get::<Win32>()
        .vfs
        .sections
        .file(file, 8192, false)
        .unwrap();
    let modules = machine.ext.get_mut::<adv_os::Modules>();
    let handle = modules
        .map_data_file(
            &mut machine.cpu,
            &mut machine.sys,
            "raw.dll",
            section.clone(),
            8192,
        )
        .unwrap();
    let base = modules.get(handle).base;
    assert_eq!(
        returned(copy_file(
            &mut env(&mut machine),
            "short.bin".into(),
            "raw.dll".into(),
            false,
            false
        )),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 1224);
    assert_eq!(profile(&mut machine, "C:\\Game\\raw.dll", "x"), 0);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 1224);
    assert_eq!(machine.cpu.read_u16(base).unwrap(), 0x5a4d);
    assert!(Rc::ptr_eq(
        &section,
        &machine
            .ext
            .get::<Win32>()
            .vfs
            .sections
            .lookup(identity)
            .unwrap()
    ));
    assert_eq!(
        machine
            .ext
            .get::<adv_os::Modules>()
            .minimum_data_file_length(&section),
        Some(8192)
    );
    assert_eq!(std::fs::read(root.join("game/raw.dll")).unwrap(), bytes);
    assert!(!root.join("data/overlay/raw.dll").exists());
}

#[test]
fn short_paths_without_aliases_keep_guest_names_and_buffer_contracts() {
    let (mut machine, root) = fixture("short-names");
    let name = "漢字 long name.txt";
    std::fs::write(root.join("game").join(name), b"x").unwrap();
    let units: Vec<_> = name.encode_utf16().chain([0]).collect();
    machine.cpu.write_u32(0x410000, 0xaabbccdd).unwrap();
    assert_eq!(
        path::alias_path(&mut env(&mut machine), name, 0, 0, true, false).unwrap(),
        units.len() as u32
    );
    assert_eq!(
        path::alias_path(&mut env(&mut machine), name, 0x410000, 1, true, false).unwrap(),
        units.len() as u32
    );
    assert_eq!(machine.cpu.read_u32(0x410000).unwrap(), 0xaabbccdd);
    assert_eq!(
        path::alias_path(
            &mut env(&mut machine),
            name,
            0x410000,
            units.len() as u32,
            true,
            false
        )
        .unwrap(),
        units.len() as u32 - 1
    );
    for (index, unit) in units.iter().enumerate() {
        assert_eq!(
            machine.cpu.read_u16(0x410000 + index as u32 * 2).unwrap(),
            *unit
        );
    }
    assert_eq!(
        path::alias_path(
            &mut env(&mut machine),
            "missing.txt",
            0x410000,
            1024,
            true,
            false
        )
        .unwrap(),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 2);
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn set_file_pointer_reports_last_error_only_for_ambiguous_and_failed_seeks() {
    let (mut machine, root) = fixture("seek-errors");
    std::fs::write(root.join("game/seek.dat"), b"seek").unwrap();
    let handle = open(
        &mut env(&mut machine),
        "seek.dat",
        0x8000_0000,
        OPEN_EXISTING,
    );
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    // From the x86 native oracle: (low, high input, method, return, high output, error).
    let high = 0x410100;
    let cases = [
        (10, None, 0, 10, None, 0x1234),
        (20, Some(0), 0, 20, Some(0), 0x1234),
        (u32::MAX, Some(0), 0, u32::MAX, Some(0), 0),
        (u32::MAX, Some(1), 0, u32::MAX, Some(1), 0),
        ((-5i32) as u32, None, 0, u32::MAX, None, 131),
    ];
    for (low, input, method, expected, output, error) in cases {
        if let Some(input) = input {
            machine.cpu.write_u32(high, input).unwrap();
        }
        args(
            &mut machine,
            &[handle, low, input.map_or(0, |_| high), method],
        );
        set_last_error(&mut env(&mut machine), 0x1234);
        let label = format!("{low:#x} {input:?}");
        assert_eq!(
            returned(set_file_pointer(&mut env(&mut machine))),
            expected,
            "{label}"
        );
        if let Some(output) = output {
            assert_eq!(machine.cpu.read_u32(high).unwrap(), output, "{label}");
        }
        assert_eq!(
            crate::util::last_error(&env(&mut machine)),
            error,
            "{label}"
        );
    }
    // SetFilePointerEx(handle, distance low, distance high, new position, method).
    for (distance, result, error) in [(7i64, 1, 0x1234), (-1, 0, 131)] {
        machine.cpu.write(high, &5u64.to_le_bytes()).unwrap();
        let distance = distance as u64;
        args(
            &mut machine,
            &[handle, distance as u32, (distance >> 32) as u32, high, 0],
        );
        set_last_error(&mut env(&mut machine), 0x1234);
        assert_eq!(
            returned(set_file_pointer_ex(&mut env(&mut machine))),
            result
        );
        let position = if result == 1 { 7 } else { 5 };
        assert_eq!(machine.cpu.read_u32(high).unwrap(), position);
        assert_eq!(crate::util::last_error(&env(&mut machine)), error);
    }
    machine.sys.objects.close(handle);
}
