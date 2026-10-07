use super::*;
use crate::Win32Config;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

const DATA: u32 = 0x410000;
const OUTPUT: u32 = DATA + 0x400;
const READ_ONLY: u32 = DATA + 0x1000;
const MARKER: u32 = 0x13572468;

fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

fn name(machine: &mut Machine<JitCpu>, path: &str) {
    let mut bytes: Vec<_> = path.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let length = bytes.len() as u16;
    bytes.extend_from_slice(&[b'X', 0, b'X', 0]);
    machine.cpu.write(DATA + 0x100, &bytes).unwrap();
    machine.cpu.write(DATA + 64, &length.to_le_bytes()).unwrap();
    // MaximumLength is not the extent of a counted input name.
    machine.cpu.write(DATA + 66, &0u16.to_le_bytes()).unwrap();
    machine.cpu.write_u32(DATA + 68, DATA + 0x100).unwrap();
    machine.cpu.write(DATA, &[0; 24]).unwrap();
    machine.cpu.write_u32(DATA, 24).unwrap();
    machine.cpu.write_u32(DATA + 8, DATA + 64).unwrap();
    machine.cpu.write_u32(DATA + 12, 0x40).unwrap();
    machine.cpu.write(OUTPUT, &[0xa5; 64]).unwrap();
}

fn record(machine: &Machine<JitCpu>) -> [u8; 64] {
    let mut result = [0; 64];
    machine.cpu.read(OUTPUT, &mut result).unwrap();
    result
}

fn query(machine: &mut Machine<JitCpu>, full_query: bool, output: u32) -> i32 {
    let mut env = env(machine);
    let status = if full_query {
        full(&mut env, GuestPtr::new(DATA), GuestPtr::new(output))
    } else {
        basic(&mut env, GuestPtr::new(DATA), GuestPtr::new(output))
    };
    assert_eq!(util::last_error(&env), MARKER);
    status
}

#[test]
fn native_path_queries_share_metadata_and_preserve_failure_outputs() {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for name in ["NtQueryAttributesFile", "NtQueryFullAttributesFile"] {
        assert!(registry.dynamic_available("ntdll.dll", name));
        assert_eq!(
            registry.get("ntdll.dll", name).unwrap().conv,
            adv_abi::Conv::Stdcall { argc: 2 }
        );
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/nt-attributes-tests")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(root.join("game/dir")).unwrap();
    std::fs::create_dir_all(root.join("resources")).unwrap();
    std::fs::write(root.join("game/字🌻.dat"), b"base").unwrap();
    std::fs::write(root.join("resources/字🌻.dat"), b"replacement").unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("resources/字🌻.dat"))
        .unwrap();
    let accessed = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567_890);
    let modified = accessed + std::time::Duration::from_secs(9);
    file.set_times(
        std::fs::FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )
    .unwrap();
    drop(file);
    let mut config = Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    config.os.resource_dir = Some(root.join("resources"));
    Win32::install(&mut machine, config).unwrap();
    machine
        .spawn("native attributes", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    util::set_last_error(&mut env(&mut machine), MARKER);
    machine
        .ext
        .get::<Win32>()
        .vfs
        .set_file_normal(r"C:\Game\字🌻.dat")
        .unwrap();

    name(&mut machine, r"\??\C:\Game\字🌻.dat");
    assert_eq!(query(&mut machine, false, OUTPUT), 0);
    let bytes = record(&machine);
    assert_eq!(
        u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
        128_790_414_900_000_000
    );
    assert_eq!(
        u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        128_790_414_990_000_000
    );
    assert_eq!(&bytes[24..32], &[0; 8]);
    assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 0x80);
    assert_eq!(&bytes[36..40], &[0; 4]);
    assert_eq!(&bytes[40..], &[0xa5; 24]);

    name(&mut machine, r"\??\C:\Game\字🌻.dat");
    assert_eq!(query(&mut machine, true, OUTPUT), NOT_SUPPORTED);
    assert_eq!(record(&machine), [0xa5; 64]);
    name(&mut machine, r"\??\C:\Game\dir");
    assert_eq!(query(&mut machine, false, OUTPUT), 0);
    assert_eq!(machine.cpu.read_u32(OUTPUT + 32).unwrap(), 0x10);

    for (path, expected) in [
        (r"\??\C:\Game\missing", NAME_NOT_FOUND),
        (r"\??\C:\Game\absent\missing", PATH_NOT_FOUND),
    ] {
        for full_query in [false, true] {
            name(&mut machine, path);
            assert_eq!(query(&mut machine, full_query, OUTPUT), expected);
            assert_eq!(record(&machine), [0xa5; 64]);
        }
    }
    name(&mut machine, r"\??\C:\Game\字🌻.dat");
    machine.cpu.write_u32(DATA, 20).unwrap();
    assert_eq!(query(&mut machine, false, OUTPUT), INVALID_PARAMETER);
    assert_eq!(record(&machine), [0xa5; 64]);
    name(&mut machine, r"\??\C:\Game\字🌻.dat");
    machine.cpu.write_u32(DATA + 4, 0x12345678).unwrap();
    assert_eq!(query(&mut machine, false, OUTPUT), INVALID_HANDLE);
    assert_eq!(record(&machine), [0xa5; 64]);
    name(&mut machine, r"\??\C:\Game\字🌻.dat");
    machine.cpu.write(DATA + 64, &3u16.to_le_bytes()).unwrap();
    assert_eq!(query(&mut machine, false, OUTPUT), NAME_INVALID);
    assert_eq!(record(&machine), [0xa5; 64]);
    name(&mut machine, r"\??\C:\Game\字🌻.dat");
    machine.cpu.write_u32(DATA + 68, READ_ONLY + 4095).unwrap();
    assert_eq!(query(&mut machine, false, OUTPUT), ACCESS_VIOLATION);
    assert_eq!(record(&machine), [0xa5; 64]);
    name(&mut machine, r"\??\C:\Game\字🌻.dat");
    machine.cpu.write(READ_ONLY, &[0x77; 64]).unwrap();
    for full_query in [false, true] {
        assert_eq!(query(&mut machine, full_query, READ_ONLY), ACCESS_VIOLATION);
        assert_eq!(
            query(&mut machine, full_query, DATA + 4096 - 24),
            ACCESS_VIOLATION
        );
    }
    let mut untouched = [0; 64];
    machine.cpu.read(READ_ONLY, &mut untouched).unwrap();
    assert_eq!(untouched, [0x77; 64]);
    assert_eq!(machine.sys.objects.hosts::<FileObj>().count(), 0);
    assert!(!root.join("data/overlay").exists());
    assert_eq!(std::fs::read(root.join("game/字🌻.dat")).unwrap(), b"base");
}
