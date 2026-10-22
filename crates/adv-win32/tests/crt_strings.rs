//! String exports share checked byte/code-unit kernels across CRT versions.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
fn machine(jit: bool) -> Machine<JitCpu> {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    adv_win32::msvcrt::register(&mut r);
    assert!(r.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-strings",
            "fixture.exe",
            "/nonexistent/crt-strings-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    m
}
fn run(m: &mut Machine<JitCpu>, a: Asm) -> RunResult {
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("CRT string fixture", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn invoke(
    m: &mut Machine<JitCpu>,
    a: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    result: u32,
) {
    let module = if module.starts_with("api-ms-win-crt-")
        && !include_str!("../../../data/abi/crt-exports.tsv")
            .lines()
            .any(|row| row.starts_with(&format!("{module}\t{name}\t")))
    {
        "ucrtbase.dll"
    } else {
        module
    };
    let api = m.resolve_import(module, name).unwrap();
    for &arg in args.iter().rev() {
        a.push_imm(arg);
    }
    a.call(api)
        .add_esp(args.len() as u8 * 4)
        .mov_mem_eax(result);
}
fn text(m: &mut Machine<JitCpu>, p: u32, s: &[u16], wide: bool) {
    for (i, &v) in s.iter().chain([0].iter()).enumerate() {
        if wide {
            m.cpu.write(p + i as u32 * 2, &v.to_le_bytes()).unwrap();
        } else {
            m.cpu.write(p + i as u32, &[v as u8]).unwrap();
        }
    }
}
fn read(m: &Machine<JitCpu>, p: u32, count: u32, wide: bool) -> Vec<u16> {
    (0..count)
        .map(|i| {
            if wide {
                m.cpu.read_u16(p + i * 2).unwrap()
            } else {
                u16::from(m.cpu.read_u8(p + i).unwrap())
            }
        })
        .collect()
}
// spec: CRT-LIB-1, CRT-LIB-2
#[test]
fn length_comparison_search_and_copy_are_shared_by_every_cataloged_runtime() {
    for jit in [false, true] {
        for module in [
            "msvcrt20.dll",
            "msvcrt40.dll",
            "msvcrt.dll",
            "msvcr70.dll",
            "msvcr71.dll",
            "msvcr80.dll",
            "msvcr90.dll",
            "msvcr100.dll",
            "msvcr110.dll",
            "msvcr120.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-string-l1-1-0.dll",
        ] {
            for wide in [false, true] {
                let mut m = machine(jit);
                let prefix = if wide { "wcs" } else { "str" };
                let width = if wide { 2 } else { 1 };
                text(&mut m, DATA + 256, &[97, 98, 97, 98, 99], wide);
                text(&mut m, DATA + 512, &[97, 98, 99], wide);
                text(&mut m, DATA + 768, &[97, 255], wide);
                text(&mut m, DATA + 896, &[97, 128], wide);
                let mut a = Asm::new(CODE);
                for (i, (op, args)) in [
                    ("len", vec![DATA + 256]),
                    ("cmp", vec![DATA + 768, DATA + 896]),
                    ("chr", vec![DATA + 256, 0]),
                    ("rchr", vec![DATA + 256, 97]),
                    ("str", vec![DATA + 256, DATA + 512]),
                    ("cpy", vec![DATA + 1024, DATA + 256]),
                    ("ncpy", vec![DATA + 1280, DATA + 512, 6]),
                    ("cat", vec![DATA + 1024, DATA + 512]),
                    ("ncat", vec![DATA + 1024, DATA + 512, 2]),
                ]
                .into_iter()
                .enumerate()
                {
                    invoke(
                        &mut m,
                        &mut a,
                        module,
                        &format!("{prefix}{op}"),
                        &args,
                        DATA + i as u32 * 4,
                    );
                }
                a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
                let result = run(&mut m, a);
                assert!(
                    matches!(result, RunResult::Exited(0)),
                    "{module} {wide} {jit}: {result:?}"
                );
                assert_eq!(m.cpu.read_u32(DATA).unwrap(), 5);
                assert!((m.cpu.read_u32(DATA + 4).unwrap() as i32) > 0);
                assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), DATA + 256 + 5 * width);
                assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), DATA + 256 + 2 * width);
                assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), DATA + 256 + 2 * width);
                assert_eq!(read(&m, DATA + 1280, 6, wide), [97, 98, 99, 0, 0, 0]);
                assert_eq!(
                    read(&m, DATA + 1024, 11, wide),
                    [97, 98, 97, 98, 99, 97, 98, 99, 97, 98, 0]
                );
            }
        }
    }
}
// spec: CRT-LIB-1, CRT-LIB-2
#[test]
fn bounded_operations_stop_at_count_and_searches_stop_before_an_unmapped_tail() {
    for jit in [false, true] {
        for wide in [false, true] {
            let mut m = machine(jit);
            let prefix = if wide { "wcs" } else { "str" };
            let width = if wide { 2 } else { 1 };
            let edge = DATA + 16384 - width;
            if wide {
                m.cpu.write(edge, &0x8123u16.to_le_bytes()).unwrap();
            } else {
                m.cpu.write(edge, &[0x81]).unwrap();
            }
            let mut a = Asm::new(CODE);
            let module = "ucrtbase.dll";
            invoke(
                &mut m,
                &mut a,
                module,
                &format!("{prefix}nlen"),
                &[edge, 1],
                DATA,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                &format!("{prefix}ncmp"),
                &[edge, edge, 1],
                DATA + 4,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                &format!("{prefix}ncpy"),
                &[DATA + 1024, edge, 1],
                DATA + 8,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                &format!("{prefix}chr"),
                &[edge, if wide { 0x8123 } else { 0x81 }],
                DATA + 12,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                &format!("{prefix}nlen"),
                &[0, 0],
                DATA + 16,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                &format!("{prefix}ncmp"),
                &[0, 0, 0],
                DATA + 20,
            );
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            let result = run(&mut m, a);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
            assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), edge);
            assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 0);
        }
    }
}
// spec: CRT-LIB-2
#[test]
fn copy_to_read_only_destination_fails_instead_of_returning_success() {
    for jit in [false, true] {
        for wide in [false, true] {
            let mut m = machine(jit);
            text(&mut m, DATA + 256, &[97, 98], wide);
            let mut a = Asm::new(CODE);
            invoke(
                &mut m,
                &mut a,
                "msvcrt.dll",
                if wide { "wcscpy" } else { "strcpy" },
                &[CODE + 2048, DATA + 256],
                DATA,
            );
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            assert!(matches!(run(&mut m, a), RunResult::Failed(_)));
        }
    }
}

#[path = "crt_strings/spans.rs"]
mod spans;

#[path = "crt_strings/edits.rs"]
mod edits;
