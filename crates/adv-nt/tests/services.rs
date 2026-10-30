#![cfg(any(target_vendor = "apple", target_os = "android"))]
//! Compare service bodies with the pinned guest implementations in isolated servers.
mod common;
#[path = "../src/native/input.rs"]
mod input_body;

struct Platform;
struct InputChecked;
#[derive(Default)]
struct TimeAudits {
    begin: [u64; 3],
    done: u8,
}
static SYSCALL_HANDLER: std::sync::OnceLock<adv_machine::Handler> = std::sync::OnceLock::new();
impl adv_machine::Resolver for Platform {
    fn provides(&self, module: &str) -> bool {
        adv_machine::Resolver::provides(&adv_nt::dispatch::Platform, module)
    }
    fn host_fn(&self, module: &str, name: &str) -> Option<adv_machine::HostFn> {
        let mut binding =
            adv_machine::Resolver::host_fn(&adv_nt::dispatch::Platform, module, name)?;
        if module == adv_nt::dispatch::MODULE && name == adv_nt::dispatch::SYSCALL {
            SYSCALL_HANDLER.get_or_init(|| binding.handler);
            binding.handler = |env| {
                use adv_cpu::backend::{GuestExt, Reg};
                let number = env.cpu.reg(Reg::Eax) as usize;
                if adv_nt::syscalls::SYSCALLS
                    .get(number)
                    .is_some_and(|&(name, _)| name == "NtTestAlert")
                {
                    let marker = env.cpu.read_u32(env.ctx().teb + 0x34).unwrap_or(0);
                    if marker == 0x5a17e13 {
                        check_input_bodies(env);
                    } else if (0x5a17e20..=0x5a17e25).contains(&marker) {
                        let index = ((marker - 0x5a17e20) / 2) as usize;
                        let calls = env.sys.host_calls;
                        let audit = env.ext.get_or_default::<TimeAudits>();
                        if marker & 1 == 0 {
                            audit.begin[index] = calls;
                        } else {
                            assert_eq!(calls - audit.begin[index], 1);
                            audit.done |= 1 << index;
                        }
                    }
                }
                SYSCALL_HANDLER.get().unwrap()(env)
            };
        }
        Some(binding)
    }
}

fn worker(trace: bool) {
    let Some((result, output)) = common::run_probe_engine(
        "services.exe",
        true,
        "null",
        Platform,
        |machine, _| {
            if trace {
                machine.sys.trace = adv_machine::Trace::to_writer(Box::new(std::io::sink()));
                machine.sys.trace.all = true;
            }
        },
        |machine| {
            assert!(machine.ext.try_get::<InputChecked>().is_some());
            assert_eq!(machine.ext.get::<TimeAudits>().done, 7);
        },
        true,
    ) else {
        return;
    };
    println!("{output}");
    assert!(
        matches!(result, adv_machine::RunResult::Exited(0)),
        "{result:?}"
    );
}

#[test]
#[ignore = "subprocess fixture"]
fn service_native() {
    worker(false);
}
#[test]
#[ignore = "subprocess fixture"]
fn service_reference() {
    worker(true);
}

// spec: NAT-BODY-2, NAT-BODY-18, EXEC-CALL-3
#[test]
fn service_bodies_match_guest_results_and_slow_cases() {
    if std::env::var_os("ADV_WINE_DIST").is_none() || std::env::var_os("ADV_WINE_PROBES").is_none()
    {
        eprintln!("skipped: ADV_WINE_DIST and ADV_WINE_PROBES are required");
        return;
    }
    let run = |name| {
        let output = std::process::Command::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/test-timeout.sh"),
        )
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env("ADV_TEST_LIMIT", "120")
        .output()
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!(
            "{name}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
            .lines()
            .filter(|line| line.starts_with("CHECK "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let reference = run("service_reference");
    let native = run("service_native");
    assert_eq!(reference.len(), 2);
    assert_eq!(native, reference);
}

fn request(
    machine: &mut adv_machine::Env<'_>,
    name: &str,
    words: &[u32],
    bytes: &[u8],
) -> [u8; 64] {
    let mut fixed = [0; 64];
    let number = adv_nt::protocol::REQUEST_NAMES
        .iter()
        .position(|&request| request == name)
        .unwrap() as u32;
    fixed[..4].copy_from_slice(&number.to_ne_bytes());
    for (index, word) in words.iter().enumerate() {
        fixed[12 + 4 * index..16 + 4 * index].copy_from_slice(&word.to_ne_bytes());
    }
    machine
        .ext
        .get_mut::<adv_nt::server::Link>()
        .call_raw(machine.context, machine.cpu, &mut fixed, &[bytes], &mut [])
        .unwrap();
    assert_eq!(
        u32::from_ne_bytes(fixed[..4].try_into().unwrap()),
        0,
        "{name}"
    );
    fixed
}

// spec: NAT-BODY-12, NAT-BODY-18
fn check_input_bodies(machine: &mut adv_machine::Env<'_>) {
    use adv_cpu::backend::Prot;
    use adv_cpu::body::{GuestMem, Scratch, Stop};
    let services = machine.sys.services(machine.ext);
    let tid = services.thread;
    assert_ne!(tid, 0);
    assert!(!adv_wineserver::has_queue(tid));
    let mut scratch = Scratch::default();
    let mut mem = GuestMem::for_body(machine.cpu, &mut scratch, services);
    assert_eq!(
        (input_body::KEY_STATE.full)(&mut mem, [0; 7]),
        Err(Stop::Guest)
    );
    assert_eq!(
        (input_body::KEYBOARD_STATE.full)(&mut mem, [0; 7]),
        Err(Stop::Guest)
    );
    assert!(!adv_wineserver::has_queue(tid));
    let name = |text: &str| {
        text.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let station = request(
        machine,
        "create_winstation",
        &[0, 0x10000000, 0, 0],
        &name(r"\Windows\WindowStations\ServiceStation"),
    );
    let station = u32::from_ne_bytes(station[8..12].try_into().unwrap());
    request(machine, "set_process_winstation", &[station], &[]);
    let desktop = request(
        machine,
        "create_desktop",
        &[0x80000000, 0x10000000, 0],
        &name("ServiceDesktop"),
    );
    let desktop = u32::from_ne_bytes(desktop[8..12].try_into().unwrap());
    request(machine, "set_thread_desktop", &[desktop], &[]);
    request(machine, "get_msg_queue", &[], &[]);
    assert!(adv_wineserver::has_queue(tid));
    let pattern: Vec<_> = (0..=255).collect();
    request(machine, "set_key_state", &[0], &pattern);
    let at = machine.sys.vm.alloc(machine.cpu, 4096, Prot::RW).unwrap();
    for key in [0, 1, 0x80, 0x81, 0xff, 0x100, u32::MAX] {
        let expected = request(machine, "get_key_state", &[0, key], &[])[8];
        let mut mem = GuestMem::for_body(machine.cpu, &mut scratch, services);
        let result = (input_body::KEY_STATE.full)(&mut mem, [key, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(result.value as u32, (expected & 0x81) as i8 as i32 as u32);
    }
    let mut mem = GuestMem::for_body(machine.cpu, &mut scratch, services);
    assert_eq!(
        (input_body::KEYBOARD_STATE.full)(&mut mem, [at, 0, 0, 0, 0, 0, 0])
            .unwrap()
            .value,
        1
    );
    let mut bytes = [0; 256];
    machine.cpu.read(at, &mut bytes).unwrap();
    assert_eq!(
        bytes.as_slice(),
        pattern.iter().map(|byte| byte & 0x81).collect::<Vec<_>>()
    );
    benchmark_input_bodies(tid);
    machine.ext.insert(InputChecked);
    use adv_cpu::backend::GuestExt;
    machine.cpu.write_u32(machine.ctx().teb + 0x34, 0).unwrap();
}

fn benchmark_input_bodies(tid: u32) {
    use adv_cpu::backend::{Guest, Prot};
    use adv_machine::{
        Clock, Conv, HostFn, Machine, Outcome, Resolver, RunResult, layout::PARK_ADDR,
    };
    struct Fixture;
    impl Resolver for Fixture {
        fn provides(&self, _: &str) -> bool {
            true
        }
        fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
            Some(HostFn {
                conv: Conv::Stdcall { argc: 1 },
                handler: |_| Outcome::Return(0),
                data: 0,
                trace: false,
                guest: None,
                leaf: None,
            })
        }
    }
    struct Thread(u32);
    for (name, body, argument, expected) in [
        (
            "NtUserGetKeyState",
            input_body::KEY_STATE,
            0x81u32,
            0xffffff81u32,
        ),
        (
            "NtUserGetKeyboardState",
            input_body::KEYBOARD_STATE,
            0x420000,
            1,
        ),
    ] {
        const CODE: u32 = 0x400000;
        const ENTRY: u32 = 0x410000;
        const CALLS: u32 = 100000;
        let mut machine = Machine::new(
            adv_cpu::Cpu::new(PARK_ADDR, true).unwrap(),
            Fixture,
            Clock::real(),
        )
        .unwrap();
        machine.ext.insert(Thread(tid));
        machine.sys.body_services =
            Some(|_, ext, services| services.thread = ext.get::<Thread>().0);
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(ENTRY, 4096, Prot::RX).unwrap();
        machine.cpu.map(0x420000, 4096, Prot::RW).unwrap();
        let original = [0xb8, 0, 0, 0, 0, 0xc2, 4, 0];
        machine.cpu.write(ENTRY, &original).unwrap();
        let thunk = machine.resolve_import("fixture", name).unwrap();
        machine
            .sys
            .redirects
            .insert(ENTRY, (thunk, original.into()));
        machine.sys.direct_bodies.insert(ENTRY, body.direct);
        let mut code = vec![0xbd];
        code.extend_from_slice(&CALLS.to_le_bytes());
        code.push(0x68);
        code.extend_from_slice(&argument.to_le_bytes());
        code.push(0xe8);
        code.extend_from_slice(&ENTRY.wrapping_sub(CODE + 15).to_le_bytes());
        code.extend_from_slice(&[0x4d, 0x0f, 0x85]);
        code.extend_from_slice(&(-17i32).to_le_bytes());
        code.push(0xc3);
        machine.cpu.write(CODE, &code).unwrap();
        for pass in 0..3 {
            machine.spawn("input body", CODE, &[], 0x10000).unwrap();
            let start = std::time::Instant::now();
            let result = machine.run();
            let elapsed = start.elapsed();
            // First visits use the fixture's zero-returning full path.
            if pass == 2 {
                assert!(
                    matches!(result, RunResult::Exited(value) if value == expected),
                    "{result:?}"
                );
                eprintln!(
                    "BENCH {name} {:.1} ns/call",
                    elapsed.as_nanos() as f64 / f64::from(CALLS)
                );
            }
        }
        assert!(machine.sys.host_calls < 16);
    }
}
