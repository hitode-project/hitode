//! Personality invariant 2 (docs/personality-contracts.md): every version
//! fact a guest can read is generated from one profile and agrees with it.
//! Channels Windows also carries the version in, but that ADV does not model
//! yet, must stay absent: whoever adds one has to generate it from the
//! profile and extend this test.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{PEB_ADDR, peb};
use adv_machine::{Clock, Machine, RunResult};
use adv_unicorn::UnicornCpu;
use adv_win32::personality::{Profile, WIN7_SP1};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const INFO_A: u32 = DATA;
const INFO_EX_A: u32 = DATA + 0x100;
const INFO_W: u32 = DATA + 0x200;
const INFO_EX_W: u32 = DATA + 0x400;
const RTL: u32 = DATA + 0x600;
const LOG: u32 = DATA + 0x800;
const REQUEST: u32 = DATA + 0xa00;
const STRINGS: u32 = DATA + 0xc00;
const OUT: u32 = DATA + 0xf00;
const KUSER_SHARED_DATA: u32 = 0x7ffe_0000;

/// A second NT workstation identity: every other field differs from WIN7_SP1.
const VISTA_SP2: Profile = Profile {
    major: 6,
    minor: 0,
    build: 6002,
    platform_id: 2,
    service_pack: (2, 0),
    csd_version: "Service Pack 2",
    suite_mask: 0x100,
    product_type: 1,
    edition: 6,
};

struct Test {
    machine: Machine<UnicornCpu>,
    code: Asm,
    next: u32,
}
impl Test {
    fn new(profile: Profile) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            UnicornCpu::new(adv_machine::layout::PARK_ADDR).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/personality-versions-fixture");
        let mut cfg = Win32Config::new("/nonexistent/personality-versions", "versions.exe", data);
        cfg.personality = profile;
        Win32::install(&mut machine, cfg).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            next: LOG,
        }
    }
    fn call(&mut self, module: &str, name: &str, args: &[u32]) -> u32 {
        for arg in args.iter().rev() {
            self.code.push_imm(*arg);
        }
        let thunk = self.machine.resolve_import(module, name).unwrap();
        let out = self.next;
        self.next += 4;
        self.code.call(thunk).mov_mem_eax(out);
        out
    }
    fn kernel32(&mut self, name: &str, args: &[u32]) -> u32 {
        self.call("kernel32.dll", name, args)
    }
    fn run(&mut self) {
        assert!(self.next <= REQUEST, "call log overlaps the request");
        self.kernel32("ExitProcess", &[0]);
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.spawn("main", CODE, &[], 0x10000).unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
    }
    fn u32(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
    fn u16(&self, address: u32) -> u16 {
        self.machine.cpu.read_u16(address).unwrap()
    }
    fn bytes(&self, address: u32, len: u32) -> Vec<u8> {
        let mut bytes = vec![0; len as usize];
        self.machine.cpu.read(address, &mut bytes).unwrap();
        bytes
    }
    /// OSVERSIONINFO[EX] fields, in declaration order.
    fn info(&self, at: u32, wide: bool, ex: bool) -> Info {
        let csd_len = if wide { 256 } else { 128 };
        let raw = self.bytes(at + 20, csd_len);
        let csd = if wide {
            let units: Vec<u16> = raw
                .chunks(2)
                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                .take_while(|&unit| unit != 0)
                .collect();
            String::from_utf16(&units).unwrap()
        } else {
            let end = raw.iter().position(|&byte| byte == 0).unwrap();
            String::from_utf8(raw[..end].to_vec()).unwrap()
        };
        let tail = at + 20 + csd_len;
        Info {
            version: (self.u32(at + 4), self.u32(at + 8), self.u32(at + 12) as u16),
            platform_id: self.u32(at + 16),
            csd,
            ex: ex.then(|| {
                (
                    (self.u16(tail), self.u16(tail + 2)),
                    self.u16(tail + 4),
                    self.bytes(tail + 6, 1)[0],
                )
            }),
        }
    }
}

#[derive(Debug, PartialEq)]
struct Info {
    version: (u32, u32, u16),
    platform_id: u32,
    csd: String,
    /// (service pack, suite mask, product type).
    ex: Option<((u16, u16), u16, u8)>,
}
impl Info {
    fn expected(profile: &Profile, ex: bool) -> Self {
        Self {
            version: (profile.major, profile.minor, profile.build),
            platform_id: profile.platform_id,
            csd: profile.csd_version.into(),
            ex: ex.then_some((
                profile.service_pack,
                profile.suite_mask,
                profile.product_type,
            )),
        }
    }
}

fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// VerSetConditionMask with VER_EQUAL for every bit in `types`.
fn equal_mask(types: u32) -> (u32, u32) {
    let mask = (0..8)
        .filter(|bit| types & (1 << bit) != 0)
        .fold(1u64 << 63, |mask, bit| mask | 1 << (3 * bit));
    (mask as u32, (mask >> 32) as u32)
}

fn check(profile: Profile) {
    let mut t = Test::new(profile);
    for (at, size) in [
        (INFO_A, 148),
        (INFO_EX_A, 156),
        (INFO_W, 276),
        (INFO_EX_W, 284),
        (RTL, 284),
    ] {
        t.machine.cpu.write_u32(at, size).unwrap();
    }
    // OSVERSIONINFOEXW request carrying the profile, written independently
    // of the APIs under test.
    let mut request = [0u8; 284];
    for (index, value) in [
        284,
        profile.major,
        profile.minor,
        u32::from(profile.build),
        profile.platform_id,
    ]
    .into_iter()
    .enumerate()
    {
        request[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    request[276..278].copy_from_slice(&profile.service_pack.0.to_le_bytes());
    request[278..280].copy_from_slice(&profile.service_pack.1.to_le_bytes());
    t.machine.cpu.write(REQUEST, &request).unwrap();
    let version_key = STRINGS;
    let system_dll = STRINGS + 0x100;
    t.machine
        .cpu
        .write(
            version_key,
            &wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
        )
        .unwrap();
    t.machine.cpu.write(system_dll, b"kernel32.dll\0").unwrap();

    let version = t.kernel32("GetVersion", &[]);
    let infos = [
        t.kernel32("GetVersionExA", &[INFO_A]),
        t.kernel32("GetVersionExA", &[INFO_EX_A]),
        t.kernel32("GetVersionExW", &[INFO_W]),
        t.kernel32("GetVersionExW", &[INFO_EX_W]),
    ];
    let rtl = t.call("ntdll.dll", "RtlGetVersion", &[RTL]);
    // MINOR | MAJOR | BUILD | PLATFORMID | SERVICEPACKMINOR | SERVICEPACKMAJOR.
    let (low, high) = equal_mask(0x3f);
    let verified = t.kernel32("VerifyVersionInfoW", &[REQUEST, 0x3f, low, high]);
    let product = t.kernel32("GetProductInfo", &[profile.major, profile.minor, 0, 0, OUT]);
    // HKEY_LOCAL_MACHINE, KEY_READ.
    let registry = t.call(
        "advapi32.dll",
        "RegOpenKeyExW",
        &[0x8000_0002, version_key, 0, 0x2_0019, OUT + 4],
    );
    let resource = t.call(
        "version.dll",
        "GetFileVersionInfoSizeA",
        &[system_dll, OUT + 8],
    );
    t.run();

    let expected_version = (u32::from(profile.build) << 16) | (profile.minor << 8) | profile.major;
    assert_eq!(t.u32(version), expected_version);
    assert!(infos.iter().all(|&result| t.u32(result) == 1));
    assert_eq!(
        t.info(INFO_A, false, false),
        Info::expected(&profile, false)
    );
    assert_eq!(
        t.info(INFO_EX_A, false, true),
        Info::expected(&profile, true)
    );
    assert_eq!(t.info(INFO_W, true, false), Info::expected(&profile, false));
    assert_eq!(
        t.info(INFO_EX_W, true, true),
        Info::expected(&profile, true)
    );
    assert_eq!(t.u32(rtl), 0);
    assert_eq!(t.info(RTL, true, true), Info::expected(&profile, true));
    let peb_info = Info {
        version: (
            t.u32(PEB_ADDR + peb::OS_MAJOR_VERSION),
            t.u32(PEB_ADDR + peb::OS_MINOR_VERSION),
            t.u16(PEB_ADDR + peb::OS_BUILD_NUMBER),
        ),
        platform_id: t.u32(PEB_ADDR + peb::OS_PLATFORM_ID),
        csd: {
            let length = u32::from(t.u16(PEB_ADDR + peb::CSD_VERSION));
            let buffer = t.u32(PEB_ADDR + peb::CSD_VERSION + 4);
            let units: Vec<u16> = t
                .bytes(buffer, length)
                .chunks(2)
                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                .collect();
            String::from_utf16(&units).unwrap()
        },
        ex: None,
    };
    assert_eq!(peb_info, Info::expected(&profile, false));
    let service_pack = t.u16(PEB_ADDR + peb::OS_CSD_VERSION);
    assert_eq!(
        ((service_pack >> 8), service_pack & 0xff),
        profile.service_pack
    );
    assert_eq!(t.u32(verified), 1);
    assert_eq!((t.u32(product), t.u32(OUT)), (1, profile.edition));

    // Deferred channels (docs/personality-contracts.md): no CurrentVersion
    // registry values, no system version resources, no KUSER_SHARED_DATA.
    assert_ne!(t.u32(registry), 0, "CurrentVersion registry key appeared");
    assert_eq!(t.u32(resource), 0, "system version resource appeared");
    assert!(
        !t.machine.cpu.accessible(KUSER_SHARED_DATA, 4, Prot::READ),
        "KUSER_SHARED_DATA is mapped"
    );
}

#[test]
// spec: PERS-PROFILE-1, PERS-PROFILE-3
fn default_profile_is_windows_7_sp1_in_every_version_fact() {
    check(WIN7_SP1);
}

#[test]
// spec: PERS-PROFILE-1
fn an_alternate_profile_changes_every_version_fact_together() {
    check(VISTA_SP2);
}

#[test]
fn verify_version_info_rejects_a_newer_major_than_the_profile() {
    let mut t = Test::new(WIN7_SP1);
    let mut request = [0u8; 284];
    request[..4].copy_from_slice(&284u32.to_le_bytes());
    request[4..8].copy_from_slice(&(WIN7_SP1.major + 1).to_le_bytes());
    t.machine.cpu.write(REQUEST, &request).unwrap();
    // VER_MAJORVERSION, VER_EQUAL.
    let (low, high) = equal_mask(0x02);
    let verified = t.kernel32("VerifyVersionInfoW", &[REQUEST, 0x02, low, high]);
    let error = t.kernel32("GetLastError", &[]);
    t.run();
    // ERROR_OLD_WIN_VERSION.
    assert_eq!((t.u32(verified), t.u32(error)), (0, 1150));
}
