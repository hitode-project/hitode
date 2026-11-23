//! `NtSetInformationThread(ThreadWineNativeThreadName)` validates the buffer
//! size and records the guest thread's name on its machine context.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock as MachineClock, Machine, Outcome};
use adv_nt::server::{Connection, Progress, Time};
use adv_wineserver::{Clock, Config, Server, State};

struct TestClock(Cell<u64>);

impl Clock for TestClock {
    fn monotonic(&self) -> u64 {
        self.0.get()
    }
    fn unix(&self) -> u64 {
        17_672_256_000_000_000 + self.0.get()
    }
}

struct OwnedRuntime {
    server: Rc<RefCell<Server>>,
    clock: Rc<TestClock>,
}

impl Progress for OwnedRuntime {
    fn run_server(&mut self, _: Time) {
        assert!(matches!(
            self.server.borrow_mut().step(self.clock.as_ref()),
            State::Waiting(_)
        ));
    }
}

// spec: NT-PROC-2
#[test]
fn set_native_thread_name_validates_size_and_updates_context_name() {
    let root = std::env::temp_dir().join(format!("adv-nt-thread-name-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let prefix = root.join("prefix");
    let nls = root.join("install/share/wine/nls");
    std::fs::create_dir_all(&prefix).unwrap();
    std::fs::create_dir_all(&nls).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::copy(
        manifest.join("../../vendor/wine/nls/l_intl.nls"),
        nls.join("l_intl.nls"),
    )
    .unwrap();

    let clock = Rc::new(TestClock(Cell::new(0)));
    let config = Config {
        prefix,
        server_dir: root.join("server"),
        install: root.join("install"),
        debug: 0,
    };
    let server = Rc::new(RefCell::new(
        Server::start(&config, clock.as_ref()).unwrap(),
    ));
    let socket = server.borrow().socket_dir().unwrap();

    let mut progress = OwnedRuntime {
        server: server.clone(),
        clock: clock.clone(),
    };
    let (connection, first) = Connection::connect(&socket, &mut progress, Time::default()).unwrap();

    let mut machine = Machine::new(
        adv_cpu::Cpu::new(PARK_ADDR, false).unwrap(),
        adv_nt::dispatch::Platform,
        MachineClock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(0x10000, 0x1000, Prot::RW).unwrap();
    machine.sys.pid = first.pid;
    let ctx = machine.spawn("initial", PARK_ADDR, &[], 0x10000).unwrap();
    machine.sys.contexts[ctx as usize].tid = first.tid;
    let link = adv_nt::server::Link::new(
        connection,
        Box::new(progress),
        Time::default(),
        ctx,
        first.tid,
        &mut machine.sys,
    );
    machine.ext.insert(link);

    // THREAD_NAME_INFORMATION: UNICODE_STRING { Length, MaximumLength, Buffer }
    let name: Vec<u16> = "UnityGfxWorker".encode_utf16().collect();
    let bytes: Vec<u8> = name.iter().flat_map(|u| u.to_le_bytes()).collect();
    let str_at = 0x10100u32;
    machine.cpu.write(str_at, &bytes).unwrap();
    let info_at = 0x10000u32;
    let len_u16 = bytes.len() as u16;
    machine
        .cpu
        .write_u32(info_at, u32::from(len_u16) | (u32::from(len_u16) << 16))
        .unwrap();
    machine.cpu.write_u32(info_at + 4, str_at).unwrap();

    let empty_at = 0x10010u32;
    machine.cpu.write_u32(empty_at, 0).unwrap();
    machine.cpu.write_u32(empty_at + 4, 0).unwrap();

    let current_thread = 0xffff_fffeu32;
    machine
        .visit_context(Some(ctx), |env| {
            let bad = adv_nt::information::set_thread(env, &[current_thread, 1000, info_at, 4]);
            assert!(matches!(bad, Outcome::Return(0xc000_0004)));

            let ok = adv_nt::information::set_thread(env, &[current_thread, 1000, info_at, 8]);
            assert!(matches!(ok, Outcome::Return(0)));
            assert_eq!(env.sys.contexts[ctx as usize].name, "UnityGfxWorker");

            let empty = adv_nt::information::set_thread(env, &[current_thread, 1000, empty_at, 8]);
            assert!(matches!(empty, Outcome::Return(0)));
            assert_eq!(env.sys.contexts[ctx as usize].name, "UnityGfxWorker");
        })
        .unwrap();

    drop(machine);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}
