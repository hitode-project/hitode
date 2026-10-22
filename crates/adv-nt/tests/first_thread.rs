//! A new process registers its first thread with the in-process server.
#![cfg(target_vendor = "apple")]

use std::cell::Cell;
use std::path::PathBuf;

use adv_nt::server::{Connection, MACHINE_I386, Progress, Time};
use adv_wineserver::{Clock, Config, Server, State};

/// Time stands still: nothing in the test waits.
struct TestClock(Cell<u64>);

impl Clock for TestClock {
    fn monotonic(&self) -> u64 {
        self.0.get()
    }
    fn unix(&self) -> u64 {
        // 2026-01-01 UTC.
        17_672_256_000_000_000 + self.0.get()
    }
}

struct Runtime<'a> {
    server: &'a mut Server,
    clock: &'a TestClock,
}

impl Progress for Runtime<'_> {
    fn run_server(&mut self, _: Time) {
        assert!(matches!(self.server.step(self.clock), State::Waiting(_)));
    }
}

#[test]
fn first_thread_gets_ids_and_the_i386_machine() {
    let root = std::env::temp_dir().join(format!("adv-nt-first-thread-{}", std::process::id()));
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

    let clock = TestClock(Cell::new(0));
    let config = Config {
        prefix,
        install: root.join("install"),
        debug: 0,
    };
    let mut server = Server::start(&config, &clock).unwrap();
    let socket = server.socket_path().unwrap();
    let mut runtime = Runtime {
        server: &mut server,
        clock: &clock,
    };

    let (_connection, first) = Connection::connect(&socket, &mut runtime, Time::default()).unwrap();
    assert_ne!(first.pid, 0);
    assert_ne!(first.tid, 0);
    assert_eq!(first.machines.first(), Some(&MACHINE_I386));
    std::fs::remove_dir_all(root).unwrap();
}
