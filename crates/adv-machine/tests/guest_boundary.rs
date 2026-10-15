//! Diagnostic guest probes count successful bodies and both fallback routes.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, GuestCode, Machine, Outcome, RunResult, layout::PARK_ADDR};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
static OWN: GuestCode = GuestCode {
    // mov eax,[esp+4]; test eax,eax; jz host; ret 4
    code: &[
        0x8b, 0x44, 0x24, 4, 0x85, 0xc0, 0x0f, 0x84, 0, 0, 0, 0, 0xc2, 4, 0,
    ],
    host: &[8],
    links: &[],
};
static LINKED: GuestCode = GuestCode {
    code: OWN.code,
    host: &[],
    links: &[(8, "fixture.dll", "<slow>")],
};
fn run(profile: bool, guest: bool) -> (Vec<u32>, Machine<JitCpu>, [u32; 2], Vec<Vec<u8>>) {
    let mut registry = Registry::default();
    for name in ["own", "linked", "<slow>"] {
        registry.add("fixture.dll", name, Conv::Stdcall { argc: 1 }, |e| {
            Outcome::Return(e.arg(0))
        });
    }
    registry.set_guest("fixture.dll", "own", &OWN);
    registry.set_guest("fixture.dll", "linked", &LINKED);
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.sys.stats.services_enabled = profile;
    m.sys.thunks.set_guest_code(&mut m.cpu, guest).unwrap();
    let targets = [
        m.resolve_import("fixture.dll", "own").unwrap(),
        m.resolve_import("fixture.dll", "linked").unwrap(),
    ];
    let indices = std::array::from_fn(|i| {
        m.sys
            .thunks
            .published_index("fixture.dll", ["own", "linked"][i])
            .unwrap()
    });
    let original = indices
        .iter()
        .map(|&i| {
            let mut bytes = vec![0; OWN.code.len()];
            m.cpu
                .read(m.sys.thunks.get(i).guest_body().unwrap(), &mut bytes)
                .unwrap();
            bytes
        })
        .collect();
    let mut code = Asm::new(CODE);
    for (i, target) in targets.iter().enumerate() {
        for (j, arg) in [5, 0].iter().enumerate() {
            code.push_imm(*arg)
                .call(*target)
                .mov_mem_eax(DATA + (i * 2 + j) as u32 * 4);
        }
    }
    code.bytes.extend_from_slice(&[0x9c, 0x58]);
    code.mov_mem_eax(DATA + 16);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("probes", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(0)),
        "profile={profile} guest={guest}: {result:?}"
    );
    let out = (0..5)
        .map(|i| m.cpu.read_u32(DATA + i * 4).unwrap())
        .collect();
    (out, m, indices, original)
}
// spec: PROC-MACH-9, PERS-EXPORT-7
#[test]
fn probes_preserve_results_flags_and_count_each_fallback_without_disabled_instructions() {
    let (plain, off, indices, _) = run(false, true);
    assert_eq!(off.sys.thunks.guest_counts(&off.cpu, indices[0]), [0; 3]);
    let (profiled, mut on, indices, original) = run(true, true);
    assert_eq!(plain, profiled);
    assert_eq!(on.sys.thunks.guest_counts(&on.cpu, indices[0]), [2, 1, 0]);
    assert_eq!(on.sys.thunks.guest_counts(&on.cpu, indices[1]), [2, 0, 1]);
    let mut csv = Vec::new();
    on.sys
        .stats
        .write_host_csv(&on.cpu, &on.sys.thunks, &mut csv)
        .unwrap();
    let csv = String::from_utf8(csv).unwrap();
    for name in ["own", "linked"] {
        let row = csv
            .lines()
            .find(|line| line.starts_with(&format!("\"fixture.dll\",\"{name}\",")))
            .unwrap();
        assert!(row.ends_with(",1,1,0.500000,0.000000"), "{row}");
    }
    assert_eq!(on.sys.host_calls, 2);
    on.sys
        .thunks
        .set_guest_counting(&mut on.cpu, false)
        .unwrap();
    for (i, &index) in indices.iter().enumerate() {
        let mut bytes = vec![0; OWN.code.len()];
        on.cpu
            .read(on.sys.thunks.get(index).guest_body().unwrap(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, original[i], "disabled fallback must be pristine");
    }
    assert_eq!(&run(true, false).0[..4], &plain[..4]);
}
