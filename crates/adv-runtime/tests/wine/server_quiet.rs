use super::*;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

// spec: SRV-EVENT-2
#[test]
fn unchanged_input_does_not_resume_server() {
    let root = std::env::temp_dir().join(format!("adv-server-quiet-{}", std::process::id()));
    let prefix = root.join("prefix");
    std::fs::create_dir_all(&prefix).unwrap();
    let install = root.join("install");
    let nls = install.join("share/wine/nls");
    std::fs::create_dir_all(&nls).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/wine/nls/l_intl.nls"),
        nls.join("l_intl.nls"),
    )
    .unwrap();
    let time = Time {
        monotonic: 0,
        unix: 17_672_256_000_000_000,
    };
    let clock = GuestClock(Cell::new(time));
    let mut server = Server::start(
        &adv_wineserver::Config {
            prefix,
            server_dir: root.join("server"),
            install,
            debug: 0,
        },
        &clock,
    )
    .unwrap();
    let wake = WakeEvent::new().unwrap();
    let server_wake = wake.clone();
    server.set_waker(Box::new(move || server_wake.signal()));
    assert!(wake.wait(Some(Duration::ZERO)).unwrap());
    let socket = server.socket_dir().unwrap();
    let mut shared = Shared {
        driver: Rc::new(RefCell::new(Driver {
            input: server.input_generation(),
            server,
            clock,
            parked: BTreeMap::new(),
            signals: vec![],
            generation: 0,
            deadline: Some(Instant::now() + Duration::from_secs(60)),
            resumes: 0,
        })),
        id: Rc::new(Cell::new(0)),
    };
    shared.step(time, None); // Consume the watcher's initial query.
    shared.driver.borrow_mut().deadline = Some(Instant::now() + Duration::from_secs(60));
    let before = shared.driver.borrow().resumes;
    for _ in 0..100 {
        shared.step(time, None);
    }
    assert_eq!(shared.driver.borrow().resumes, before);
    shared.driver.borrow_mut().deadline = Some(Instant::now());
    shared.step(time, None);
    assert_eq!(shared.driver.borrow().resumes, before + 1);
    let directory = std::fs::File::open(&socket).unwrap();
    let socket = std::thread::spawn(move || {
        #[cfg(target_vendor = "apple")]
        unsafe extern "C" {
            fn pthread_fchdir_np(fd: i32) -> i32;
        }
        // SAFETY: only this connector thread changes its working directory.
        #[cfg(target_vendor = "apple")]
        assert_eq!(unsafe { pthread_fchdir_np(directory.as_raw_fd()) }, 0);
        #[cfg(target_os = "android")]
        unsafe {
            assert_eq!(libc::unshare(libc::CLONE_FS), 0);
            assert_eq!(libc::fchdir(directory.as_raw_fd()), 0);
        }
        UnixStream::connect("socket").unwrap()
    })
    .join()
    .unwrap();
    assert!(wake.wait(Some(Duration::from_secs(2))).unwrap());
    assert!(shared.driver.borrow().server.kernel_ready());
    let before = shared.driver.borrow().resumes;
    let (connection, thread) =
        adv_nt::server::Connection::from_socket(socket, &mut shared, time).unwrap();
    assert_ne!(thread.tid, 0);
    let input = shared.driver.borrow().server.input_generation();
    drop(connection);
    assert_ne!(shared.driver.borrow().server.input_generation(), input);
    shared.step(time, None);
    assert!(shared.driver.borrow().resumes > before);
    drop(shared);
    std::fs::remove_dir_all(root).unwrap();
}
