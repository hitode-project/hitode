//! Game Loop runner for cloud devices. It runs every test of every packaged
//! GPU test binary (`libtest_<name>.so`) in its own process, first with every
//! modern Vulkan feature masked and then as detected, so a crash in one test
//! never hides another. A test failing as detected reruns with each modern
//! feature masked alone, naming the features it needs masked. Output lands in
//! the app's external files directory, which Test Lab pulls; one summary line
//! per run also goes to logcat. The device's capabilities per tier are written
//! last, after every test has run. An `only` file in that directory narrows a
//! local run to tests whose `binary::name` contains one of its lines.

#[cfg(target_os = "android")]
mod android {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// Tests that exceed this are killed and reported as timeouts.
    const TIMEOUT: Duration = Duration::from_secs(600);
    /// `ADV_GPU_TEST_DISABLE` per tier, the compatibility tier first.
    const TIERS: [(&str, &str); 2] = [("compat", "all"), ("modern", "")];

    // The prefix of ANativeActivity used here, from android/native_activity.h.
    #[repr(C)]
    pub struct Activity {
        callbacks: *mut c_void,
        vm: *mut c_void,
        env: *mut c_void,
        class: *mut c_void,
        internal_path: *const c_char,
        external_path: *const c_char,
    }
    #[repr(C)]
    struct DlInfo {
        fname: *const c_char,
        fbase: *mut c_void,
        sname: *const c_char,
        saddr: *mut c_void,
    }
    unsafe extern "C" {
        fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
    }
    #[link(name = "android")]
    unsafe extern "C" {
        fn ANativeActivity_finish(activity: *mut Activity);
        fn ANativeActivity_setWindowFlags(activity: *mut Activity, add: u32, remove: u32);
    }
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    fn log(line: &str) {
        let text = CString::new(line).unwrap_or_default();
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe { __android_log_write(4, c"gpu-check".as_ptr(), text.as_ptr()) };
    }

    /// The directory holding this library and the test binaries beside it.
    fn libraries() -> PathBuf {
        let mut info = DlInfo {
            fname: std::ptr::null(),
            fbase: std::ptr::null_mut(),
            sname: std::ptr::null(),
            saddr: std::ptr::null_mut(),
        };
        // SAFETY: the address is a function of this library.
        let found = unsafe { dladdr(ANativeActivity_onCreate as *const c_void, &mut info) };
        assert!(found != 0 && !info.fname.is_null(), "dladdr");
        // SAFETY: dladdr returned a NUL-terminated path.
        let path = unsafe { CStr::from_ptr(info.fname) }.to_string_lossy();
        Path::new(path.as_ref())
            .parent()
            .expect("library directory")
            .to_path_buf()
    }

    fn run(out: &Path) {
        let mut tests: Vec<(String, PathBuf)> = std::fs::read_dir(libraries())
            .expect("library directory")
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let name = path.file_name()?.to_str()?;
                let name = name
                    .strip_prefix("libtest_")?
                    .strip_suffix(".so")?
                    .to_owned();
                Some((name, path))
            })
            .collect();
        tests.sort();
        let tmp = out.join("tmp");
        std::fs::create_dir_all(&tmp).expect("temporary directory");
        let only: Vec<String> = std::fs::read_to_string(out.join("only"))
            .map(|s| {
                s.lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut summary = String::new();
        for (tier, disable) in TIERS {
            for (name, path) in &tests {
                let cases: Vec<_> = list(path)
                    .into_iter()
                    .filter(|case| {
                        let full = format!("{name}::{case}");
                        only.is_empty() || only.iter().any(|o| full.contains(o.as_str()))
                    })
                    .collect();
                if cases.is_empty() {
                    continue;
                }
                let file = std::fs::File::create(out.join(format!("{tier}-{name}.log")))
                    .expect("log file");
                let start = Instant::now();
                let mut failed = Vec::new();
                for case in &cases {
                    let status = run_case(path, case, disable, &tmp, &file);
                    if status != "exit status: 0" {
                        let culprits = if disable.is_empty() {
                            culprits(path, case, &tmp, &file)
                        } else {
                            String::new()
                        };
                        failed.push(format!("{case} ({status}){culprits}"));
                    }
                }
                let line = format!(
                    "{tier} {name}: {}/{} passed in {:.1} s{}",
                    cases.len() - failed.len(),
                    cases.len(),
                    start.elapsed().as_secs_f32(),
                    if failed.is_empty() {
                        String::new()
                    } else {
                        format!("; failed: {}", failed.join(", "))
                    }
                );
                log(&line);
                writeln!(summary, "{line}").unwrap();
                std::fs::write(out.join("summary.txt"), &summary).expect("summary");
            }
        }
        let mut caps = String::new();
        for (tier, disable) in [
            ("compat", adv_gpu::Modern::all()),
            ("modern", adv_gpu::Modern::empty()),
        ] {
            let options = adv_gpu::queue::DeviceOptions {
                disable,
                label: "gpu check",
                ..Default::default()
            };
            match adv_gpu::Device::open(&options) {
                Ok((device, _queue)) => {
                    let c = device.caps();
                    writeln!(
                        caps,
                        "{tier}: {} / {}\n  modern: {:?}\n  features: {:?}\n  limits: {:?}",
                        c.adapter, c.driver, c.modern, c.features, c.limits
                    )
                    .unwrap();
                }
                Err(error) => writeln!(caps, "{tier}: open failed: {error}").unwrap(),
            }
        }
        caps.push_str(&native_properties());
        std::fs::write(out.join("caps.txt"), &caps).expect("caps");
        log(caps.lines().next().unwrap_or_default());
        log("done");
    }

    /// The test names of a libtest binary.
    fn list(path: &Path) -> Vec<String> {
        let output = Command::new(path)
            .args(["--list", "--format", "terse"])
            .output()
            .expect("list tests");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_suffix(": test"))
            .map(str::to_owned)
            .collect()
    }
    /// The modern features whose masking alone makes a failing test pass.
    fn culprits(path: &Path, case: &str, tmp: &Path, log: &std::fs::File) -> String {
        let passing: Vec<_> = adv_gpu::Modern::all()
            .iter_names()
            .filter(|(name, _)| run_case(path, case, name, tmp, log) == "exit status: 0")
            .map(|(name, _)| name)
            .collect();
        if passing.is_empty() {
            " [fails without each feature]".to_owned()
        } else {
            format!(" [passes without {}]", passing.join(" or "))
        }
    }
    /// One test in its own process, so a crash never hides the others.
    fn run_case(path: &Path, case: &str, disable: &str, tmp: &Path, log: &std::fs::File) -> String {
        let start = Instant::now();
        let mut child = Command::new(path)
            .args(["--exact", case, "--test-threads=1"])
            .env("ADV_GPU_TEST_DISABLE", disable)
            .env("ADV_GPU_REQUIRE_BACKEND", "1")
            .env("RUST_BACKTRACE", "1")
            .env("TMPDIR", tmp)
            .current_dir(tmp)
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("log file"))
            .stderr(log.try_clone().expect("log file"))
            .spawn()
            .expect("spawn test");
        loop {
            if let Some(status) = child.try_wait().expect("wait test") {
                return format!("{status}");
            }
            if start.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return "timeout".to_owned();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Every memory type, and the features of formats whose caps fold several
    /// bits, which the device caps do not show.
    fn native_properties() -> String {
        use ash::vk;
        let mut out = String::new();
        let entry = unsafe { ash::Entry::load() }.expect("Vulkan loader");
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
        let info = vk::InstanceCreateInfo::default().application_info(&app);
        let instance = unsafe { entry.create_instance(&info, None) }.expect("instance");
        for device in unsafe { instance.enumerate_physical_devices() }.expect("devices") {
            let memory = unsafe { instance.get_physical_device_memory_properties(device) };
            for (i, ty) in memory.memory_types_as_slice().iter().enumerate() {
                writeln!(
                    out,
                    "memory type {i}: heap {} {:?}",
                    ty.heap_index, ty.property_flags
                )
                .unwrap();
            }
            for format in [
                vk::Format::G8_B8R8_2PLANE_420_UNORM,
                vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
                vk::Format::A4R4G4B4_UNORM_PACK16_EXT,
            ] {
                let features =
                    unsafe { instance.get_physical_device_format_properties(device, format) }
                        .optimal_tiling_features;
                writeln!(out, "format {format:?}: {features:?}").unwrap();
            }
        }
        unsafe { instance.destroy_instance(None) };
        out
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ANativeActivity_onCreate(
        activity: *mut Activity,
        _saved: *mut c_void,
        _size: usize,
    ) {
        // SAFETY: the platform passes a live activity with a NUL-terminated
        // external path.
        let out = PathBuf::from(
            unsafe { CStr::from_ptr((*activity).external_path) }
                .to_string_lossy()
                .into_owned(),
        );
        // AWINDOW_FLAG_KEEP_SCREEN_ON: a sleeping screen throttles the run.
        unsafe { ANativeActivity_setWindowFlags(activity, 0x80, 0) };
        let activity = activity as usize;
        std::thread::spawn(move || {
            run(&out);
            // SAFETY: finishing may be requested from any thread; the
            // activity lives until the platform destroys it after this call.
            unsafe { ANativeActivity_finish(activity as *mut Activity) };
        });
    }
}
