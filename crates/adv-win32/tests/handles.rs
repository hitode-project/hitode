//! CloseHandle and DuplicateHandle work the same for every kernel object in the table.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{PAGE, PARK_ADDR};
use adv_machine::{Clock, Machine, Object, ObjectLimits, Objects, RunResult};
use adv_unicorn::UnicornCpu;
use adv_win32::{FileObj, Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const PATH: u32 = DATA + 0x200;
const NAME: u32 = DATA + 0x280;
const OUT: u32 = DATA + 0x300;
const CURRENT_PROCESS: u32 = u32::MAX;
const CURRENT_THREAD: u32 = u32::MAX - 1;

enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored, Value};

struct Test {
    m: Machine<UnicornCpu>,
    a: Asm,
}

impl Test {
    fn new(name: &str) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        adv_win32::compat::register(&mut registry);
        let mut m = Machine::new(
            UnicornCpu::new(PARK_ADDR).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let directory = std::env::temp_dir().join(format!("adv-handle-tests-{name}"));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("game")).unwrap();
        std::fs::write(directory.join("game/data.bin"), b"0123456789").unwrap();
        let cfg = Win32Config::new(
            directory.join("game"),
            "fixture.exe",
            directory.join("data"),
        );
        Win32::install(&mut m, cfg).unwrap();
        m.cpu.map(CODE, PAGE, Prot::RX).unwrap();
        m.cpu.map(DATA, PAGE, Prot::RW).unwrap();
        m.cpu.write(PATH, b"data.bin\0").unwrap();
        m.cpu.write(NAME, b"adv-handle-section\0").unwrap();
        Self {
            m,
            a: Asm::new(CODE),
        }
    }

    /// Call `name` and store its result at `save`.
    fn call(&mut self, name: &str, args: &[Arg], save: u32) -> &mut Self {
        self.call_in("kernel32.dll", name, args, save)
    }
    fn call_in(&mut self, dll: &str, name: &str, args: &[Arg], save: u32) -> &mut Self {
        for arg in args.iter().rev() {
            match arg {
                Value(value) => {
                    self.a.push_imm(*value);
                }
                Stored(address) => {
                    self.a.mov_eax_mem(*address).push_eax();
                }
            }
        }
        let thunk = self.m.resolve_import(dll, name).unwrap();
        self.a.call(thunk).mov_mem_eax(save);
        self
    }

    fn open(&mut self, save: u32) -> &mut Self {
        // GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL
        let args = [0x8000_0000, 1, 0, 3, 0x80, 0].map(Value);
        let mut all = vec![Value(PATH)];
        all.extend(args);
        self.call("CreateFileA", &all, save)
    }

    fn duplicate(&mut self, process: u32, source: Arg, out: u32, options: u32) -> &mut Self {
        let args = [
            Value(process),
            source,
            Value(CURRENT_PROCESS),
            Value(out),
            Value(0),
            Value(0),
            Value(options),
        ];
        self.call("DuplicateHandle", &args, OUT + 0xfc)
    }

    fn run(&mut self) {
        let exit = self
            .m
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        self.a.push_imm(0).call(exit);
        self.m.cpu.write(CODE, &self.a.bytes).unwrap();
        self.m.spawn("main", CODE, &[], 0x1_0000).unwrap();
        let result = self.m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }

    fn word(&self, address: u32) -> u32 {
        self.m.cpu.read_u32(address).unwrap()
    }

    fn bytes(&self, address: u32, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        self.m.cpu.read(address, &mut bytes).unwrap();
        bytes
    }
}

#[test]
// spec: PERS-SCOPE-4
fn exhausted_object_table_reports_api_errors_without_side_effects_or_invalid_waits() {
    let mut t = Test::new("exhausted");
    t.m.sys.objects = Objects::with_limits(ObjectLimits {
        objects: 1,
        ..ObjectLimits::default()
    });
    t.m.cpu.write_u32(OUT + 0x80, 0xabcdef01).unwrap();
    t.call(
        "CreateEventA",
        &[Value(0), Value(0), Value(0), Value(NAME)],
        OUT,
    )
    .call("GetLastError", &[], OUT + 4)
    .call(
        "CreateFileA",
        &[
            Value(PATH),
            Value(0x40000000),
            Value(0),
            Value(0),
            Value(2),
            Value(0),
            Value(0),
        ],
        OUT + 8,
    )
    .call("GetLastError", &[], OUT + 12)
    .call(
        "CreateFileMappingA",
        &[
            Value(u32::MAX),
            Value(0),
            Value(4),
            Value(0),
            Value(4096),
            Value(NAME),
        ],
        OUT + 16,
    )
    .call("GetLastError", &[], OUT + 20)
    .call("CreateToolhelp32Snapshot", &[Value(4), Value(0)], OUT + 24)
    .call("GetLastError", &[], OUT + 28)
    .duplicate(CURRENT_PROCESS, Value(CURRENT_PROCESS), OUT + 0x80, 2)
    .call("GetLastError", &[], OUT + 32)
    .call_in(
        "user32.dll",
        "MsgWaitForMultipleObjects",
        &[Value(0), Value(0), Value(0), Value(10), Value(0xff)],
        OUT + 36,
    )
    .call("GetLastError", &[], OUT + 40)
    .call_in(
        "winmm.dll",
        "mmioOpenA",
        &[Value(PATH), Value(DATA), Value(0)],
        OUT + 44,
    );
    t.run();
    for offset in [4, 12, 20, 28, 32, 40] {
        assert_eq!(t.word(OUT + offset), 8, "error at {offset}");
    }
    for offset in [0, 16, 44, 0xfc] {
        assert_eq!(t.word(OUT + offset), 0, "return at {offset}");
    }
    for offset in [8, 24, 36] {
        assert_eq!(t.word(OUT + offset), u32::MAX, "return at {offset}");
    }
    assert_eq!(t.word(OUT + 0x80), 0xabcdef01);
    assert_eq!(t.word(DATA + 12), 258);
    let cfg = &t.m.ext.get::<Win32>().cfg;
    assert!(
        !cfg.os.data_dir.join("overlay/data.bin").exists(),
        "failed CreateFile must not truncate/copy the overlay"
    );
    assert_eq!(t.m.sys.objects.object_count(), 1);
}

#[test]
fn timer_exhaustion_and_partial_message_wake_leave_api_resource_baseline_unchanged() {
    let mut t = Test::new("partial-wake");
    t.m.sys.objects = Objects::with_limits(ObjectLimits {
        objects: 2,
        ..ObjectLimits::default()
    });
    t.m.sys.thunks.resolver_mut::<Registry>().unwrap().add(
        "test.dll",
        "object_count",
        adv_machine::Conv::Stdcall { argc: 0 },
        |env| adv_machine::Outcome::Return(env.sys.objects.object_count() as u32),
    );
    // One free slot lets the timer payload be charged before its first service
    // event fails; MsgWait's first event is allocated before its second fails.
    // Timer's later partial-event stages are covered by timer_lifetime.rs.
    t.call_in("test.dll", "object_count", &[], OUT + 12);
    t.call_in(
        "winmm.dll",
        "timeSetEvent",
        &[Value(10), Value(1), Value(CODE), Value(0), Value(0)],
        OUT,
    )
    .call_in(
        "user32.dll",
        "MsgWaitForMultipleObjects",
        &[Value(0), Value(0), Value(0), Value(10), Value(0xff)],
        OUT + 4,
    )
    .call("GetLastError", &[], OUT + 8)
    .call_in("test.dll", "object_count", &[], OUT + 16);
    t.run();
    assert_eq!(t.word(OUT), 0);
    assert_eq!(t.word(OUT + 4), u32::MAX);
    assert_eq!(t.word(OUT + 8), 8);
    assert_eq!(t.word(OUT + 12), 1, "one free slot before either API");
    assert_eq!(t.word(OUT + 16), t.word(OUT + 12), "API rollback baseline");
    // No DLL/TLS notification is pending: ExitProcess runs cleanup directly,
    // without charging a loader mutex after the API rollback baseline.
    assert_eq!(t.m.sys.objects.object_count(), t.word(OUT + 16) as usize);
    assert_eq!(
        (0x104..0x140)
            .step_by(4)
            .filter(|&h| matches!(t.m.sys.objects.get(h), Some(Object::Mutex { .. })))
            .count(),
        0
    );
    assert_eq!(t.m.sys.contexts.len(), 1);
}

#[test]
fn duplicated_file_handles_share_one_file_until_the_last_closes() {
    let mut t = Test::new("file");
    t.open(OUT)
        .duplicate(CURRENT_PROCESS, Stored(OUT), OUT + 4, 2)
        .call(
            "ReadFile",
            &[Stored(OUT), Value(DATA), Value(4), Value(OUT + 8), Value(0)],
            OUT + 12,
        )
        .call("CloseHandle", &[Stored(OUT)], OUT + 16)
        .call(
            "ReadFile",
            &[
                Stored(OUT + 4),
                Value(DATA + 4),
                Value(4),
                Value(OUT + 8),
                Value(0),
            ],
            OUT + 20,
        )
        .call("CloseHandle", &[Stored(OUT + 4)], OUT + 24)
        .call("CloseHandle", &[Stored(OUT + 4)], OUT + 28)
        .call("GetLastError", &[], OUT + 32);
    t.run();
    assert_ne!(t.word(OUT), u32::MAX);
    assert_ne!(t.word(OUT + 4), t.word(OUT));
    assert_eq!(t.word(OUT + 0xfc), 1, "DuplicateHandle");
    // The duplicate shares the file position: it reads on where the original stopped.
    assert_eq!(t.bytes(DATA, 8), b"01234567");
    assert_eq!(
        [t.word(OUT + 12), t.word(OUT + 16), t.word(OUT + 20)],
        [1; 3]
    );
    assert_eq!(t.word(OUT + 24), 1, "closing the last handle");
    assert_eq!(t.word(OUT + 28), 0, "a closed handle is gone");
    assert_eq!(t.word(OUT + 32), 6);
    assert_eq!(t.m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn close_source_option_closes_the_source_handle() {
    let mut t = Test::new("close-source");
    t.open(OUT)
        .duplicate(CURRENT_PROCESS, Stored(OUT), OUT + 4, 3)
        .call("CloseHandle", &[Stored(OUT)], OUT + 8)
        .call("GetFileSize", &[Stored(OUT + 4), Value(0)], OUT + 12)
        // Without an output pointer nothing is duplicated, but the source still closes.
        .duplicate(CURRENT_PROCESS, Stored(OUT + 4), 0, 1)
        .call("GetFileSize", &[Stored(OUT + 4), Value(0)], OUT + 16);
    t.run();
    assert_eq!(t.word(OUT + 8), 0, "the source was already closed");
    assert_eq!(t.word(OUT + 12), 10);
    assert_eq!(t.word(OUT + 16), u32::MAX);
    assert_eq!(t.m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn pseudo_handles_duplicate_into_real_handles() {
    let mut t = Test::new("pseudo");
    t.duplicate(CURRENT_PROCESS, Value(CURRENT_THREAD), OUT, 2)
        .call("WaitForSingleObject", &[Stored(OUT), Value(0)], OUT + 4)
        .duplicate(CURRENT_PROCESS, Value(CURRENT_PROCESS), OUT + 8, 2)
        .call("CloseHandle", &[Stored(OUT + 8)], OUT + 12)
        .duplicate(0x1234, Value(CURRENT_THREAD), OUT + 16, 2)
        .call("GetLastError", &[], OUT + 20);
    t.run();
    let thread = t.word(OUT);
    assert!(matches!(
        t.m.sys.objects.get(thread),
        Some(Object::Thread { .. })
    ));
    assert_eq!(t.word(OUT + 4), 0x102, "a running thread is not signaled");
    assert_eq!(t.word(OUT + 12), 1);
    assert_eq!(t.word(OUT + 0xfc), 0, "only this process can be named");
    assert_eq!(t.word(OUT + 20), 6);
}

#[test]
fn a_duplicated_section_keeps_its_name_bound() {
    let mut t = Test::new("section");
    // INVALID_HANDLE_VALUE, no attributes, PAGE_READWRITE, one page
    t.call(
        "CreateFileMappingA",
        &[u32::MAX, 0, 4, 0, 0x1000, NAME].map(Value),
        OUT,
    )
    .duplicate(CURRENT_PROCESS, Stored(OUT), OUT + 4, 2)
    .call("CloseHandle", &[Stored(OUT)], OUT + 8)
    .call("OpenFileMappingA", &[4, 0, NAME].map(Value), OUT + 12)
    .call("CloseHandle", &[Stored(OUT + 4)], OUT + 16)
    .call("CloseHandle", &[Stored(OUT + 12)], OUT + 20)
    .call("OpenFileMappingA", &[4, 0, NAME].map(Value), OUT + 24);
    t.run();
    assert_ne!(t.word(OUT), 0);
    assert_eq!(t.word(OUT + 8), 1);
    assert_ne!(t.word(OUT + 12), 0, "the duplicate keeps the section open");
    assert_eq!([t.word(OUT + 16), t.word(OUT + 20)], [1, 1]);
    assert_eq!(t.word(OUT + 24), 0, "the name goes with the last handle");
}
