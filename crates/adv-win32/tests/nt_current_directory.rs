use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, Reg},
};
use adv_machine::{
    Clock, Env, Machine, Outcome,
    layout::{PARK_ADDR, teb},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const MARKER: u32 = 0x12345678;

fn invoke<B: Backend>(m: &mut Machine<B>, name: &str, args: &[u32]) -> u32 {
    let entry = m.resolve_import("ntdll.dll", name).unwrap();
    let index = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let handler = m.sys.thunks.get(index).f.handler;
    m.cpu.set_reg(Reg::Esp, DATA + 2048);
    for (i, &value) in args.iter().enumerate() {
        m.cpu.write_u32(DATA + 2052 + i as u32 * 4, value).unwrap();
    }
    let error = m.sys.context(0).teb + teb::LAST_ERROR;
    m.cpu.write_u32(error, MARKER).unwrap();
    let result = handler(&mut Env {
        context: 0,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: index,
    });
    assert_eq!(m.cpu.read_u32(error).unwrap(), MARKER);
    let Outcome::Return(result) = result else {
        panic!("directory call did not return")
    };
    result
}

fn contract<B: Backend>(cpu: B, backend: &str) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/nt-current-directory-tests")
        .join(format!("{}-{backend}", std::process::id()));
    std::fs::create_dir_all(root.join("cwd-字")).unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for (name, argc) in [
        ("RtlSetCurrentDirectory_U", 1),
        ("RtlGetCurrentDirectory_U", 2),
    ] {
        assert_eq!(
            registry.get("ntdll.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
        assert!(registry.dynamic_available("ntdll.dll", name));
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(&root, "fixture.exe", root.join("data")),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.spawn("directory", CODE, &[], 65536).unwrap();
    let units: Vec<_> = "cwd-字"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(DATA + 64, &units).unwrap();
    m.cpu.write_u32(DATA, 10 | (12 << 16)).unwrap();
    m.cpu.write_u32(DATA + 4, DATA + 64).unwrap();
    let old = m.ext.get::<Win32>().vfs.cwd.clone();
    assert_eq!(invoke(&mut m, "RtlSetCurrentDirectory_U", &[DATA]), 0);
    assert_eq!(m.ext.get::<Win32>().vfs.cwd, format!("{old}\\cwd-字"));
    let expected: Vec<_> = m.ext.get::<Win32>().vfs.cwd.encode_utf16().collect();
    let length = expected.len() as u32 * 2;
    m.cpu.write(DATA + 256, &[0x55; 128]).unwrap();
    for cap in [0, length, length + 1] {
        assert_eq!(
            invoke(&mut m, "RtlGetCurrentDirectory_U", &[cap, DATA + 256]),
            length + 2
        );
        assert_eq!(m.cpu.read_u16(DATA + 256).unwrap(), 0x5555);
    }
    assert_eq!(
        invoke(
            &mut m,
            "RtlGetCurrentDirectory_U",
            &[length + 2, DATA + 256]
        ),
        length
    );
    assert_eq!(m.cpu.read_u16(DATA + 256 + length).unwrap(), 0);
    assert_eq!(m.cpu.read_u16(DATA + 256 + length + 2).unwrap(), 0x5555);
    assert_eq!(invoke(&mut m, "RtlSetCurrentDirectory_U", &[0]), 0xc0000005);
    m.cpu.write_u32(DATA + 4, 0).unwrap();
    assert_eq!(
        invoke(&mut m, "RtlSetCurrentDirectory_U", &[DATA]),
        0xc0000005
    );
    assert!(m.ext.get::<Win32>().vfs.cwd.ends_with("\\cwd-字"));
    m.cpu.write_u32(DATA, 0).unwrap();
    assert_eq!(
        invoke(&mut m, "RtlSetCurrentDirectory_U", &[DATA]),
        0xc0000033
    );
    assert_eq!(
        invoke(
            &mut m,
            "RtlGetCurrentDirectory_U",
            &[length + 2, READ_ONLY]
        ),
        0
    );
}

#[test]
fn rtl_byte_capacities_fault_preflight_and_vfs_state_in_three_backends() {
    contract(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    contract(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
    contract(JitCpu::new(PARK_ADDR, true).unwrap(), "interpreter");
}
