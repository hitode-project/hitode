//! Real x86 PE tests built from repository-owned sources, with no game assets.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn fixtures() -> &'static Path {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = root.join("target/fixtures-tests");
        let result = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("tools/fixtures/build.ps1"))
            .arg("-OutputDirectory")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "fixture build failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        output
    })
}

fn number(report: &str, key: &str) -> f64 {
    let marker = format!("\"{key}\": ");
    report
        .split(&marker)
        .nth(1)
        .unwrap_or_else(|| panic!("missing report field: {key}"))
        .split([',', '\n'])
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn generated_stubs_stop_strict_and_report_the_discovery_chain() {
    let directory = fixtures();
    for backend in ["jit", "interpreter"] {
        for mode in ["strict", "discovery"] {
            let report_path = directory.join(format!("api-stubs-{backend}-{mode}.json"));
            let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
                .arg(directory.join("fixture_api_stubs.exe"))
                .args([
                    "--headless",
                    "--backend",
                    backend,
                    "--api-mode",
                    mode,
                    "--seconds",
                    "5",
                    "--report",
                ])
                .arg(&report_path)
                .env("ADV_AUDIO_MUTE_DEVICE", "1")
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                mode == "discovery",
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let report = std::fs::read_to_string(report_path).unwrap();
            assert!(report.contains(&format!("\"mode\": \"{mode}\"")));
            assert!(report.contains("\"acceptance_eligible\": false"));
            assert!(report.contains("\"signature_conflicts\": []"));
            assert!(report.contains("\"dwrite.dll!DWriteCreateFactory\": 1"));
            assert_eq!(
                number(&report, "stub_calls"),
                if mode == "strict" { 1.0 } else { 2.0 }
            );
            if mode == "discovery" {
                assert!(report.contains("\"d2d1.dll!D2D1CreateFactory\": 1"));
                assert_eq!(number(&report, "guest_exit_code"), 0.0);
            } else {
                assert!(report.contains("strict ABI stub"));
                assert!(!report.contains("\"d2d1.dll!D2D1CreateFactory\": 1"));
            }
        }
    }
}

fn execute(name: &str, backend: &str, hidden: bool) -> (String, Vec<u8>) {
    let directory = fixtures();
    let report_path = directory.join(format!("{name}.json"));
    let png_path = directory.join(format!("{name}.png"));
    let trace_path = directory.join(format!("{name}.trace"));
    let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .arg(directory.join("fixture_window.exe"))
        .args([
            "--backend",
            backend,
            if hidden { "--hidden" } else { "--headless" },
            "--self-test-input",
            "--seconds",
            "60",
            "--width",
            "320",
            "--height",
            "240",
        ])
        .arg("--report")
        .arg(&report_path)
        .arg("--screenshot")
        .arg(&png_path)
        .arg("--trace")
        .arg(&trace_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = std::fs::read_to_string(report_path).unwrap();
    assert!(report.contains("\"stop_reason\": \"guest_exit\""));
    assert_eq!(number(&report, "guest_exit_code"), 0.0);
    assert_eq!(number(&report, "input_messages"), 5.0);
    assert!(number(&report, "frames") >= 7.0);
    assert!(number(&report, "first_frame_seconds") > 0.0);
    assert!(number(&report, "first_frame_seconds") <= number(&report, "elapsed_seconds"));
    assert!(report.contains("\"missing_apis\": []"));
    assert_eq!(number(&report, "guest_contexts"), 2.0);
    assert_eq!(number(&report, "tls_images"), 2.0);
    let trace = std::fs::read_to_string(trace_path).unwrap();
    for api in [
        "kernel32.dll!LoadLibraryA",
        "kernel32.dll!GetProcAddress",
        "kernel32.dll!CreateThread",
        "kernel32.dll!WaitForSingleObject",
        "kernel32.dll!Sleep",
        "user32.dll!BeginPaint",
        "gdi32.dll!BitBlt",
    ] {
        assert!(trace.contains(api), "execution trace did not contain {api}");
    }
    let decoder = png::Decoder::new(std::fs::File::open(png_path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((info.width, info.height), (320, 240));
    pixels.truncate(info.buffer_size());
    // Three white squares respectively prove key delivery, mouse delivery and
    // a timer guest callback continuing after a blocked Sleep.
    for x in [20, 52, 84] {
        let offset = (20 * 320 + x) * 4;
        assert_eq!(&pixels[offset..offset + 4], &[255, 255, 255, 255]);
    }
    (report, pixels)
}

#[test]
fn real_pe_jit_matches_interpreter() {
    let (_, reference) = execute("interpreter", "interpreter", false);
    let (report, jit) = execute("jit", "jit", false);
    assert_eq!(
        jit, reference,
        "JIT and Interpreter guest GDI output differs"
    );
    assert!(number(&report, "native_entries") > 0.0);
    assert!(number(&report, "compilations") > 0.0);
}

#[test]
fn audio_is_opt_in_and_headless_does_not_request_a_device() {
    let directory = fixtures();
    for enabled in [false, true] {
        let report_path = directory.join(format!("headless-audio-{enabled}.json"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_adv-run"));
        command
            .arg(directory.join("fixture_wait.exe"))
            .args(["--headless", "--seconds", "0.05", "--report"])
            .arg(&report_path);
        if enabled {
            command.arg("--audio");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = std::fs::read_to_string(report_path).unwrap();
        assert!(report.contains(&format!("Audio: enabled {enabled}")));
        assert!(report.contains("\"device_requested\": false"));
        assert!(report.contains("\"stop_reason\": \"time_limit\""));
        assert_eq!(number(&report, "frames"), 0.0);
    }
}

fn execute_audio(name: &str, backend: &str, native: bool) -> String {
    let directory = fixtures();
    let report_path = directory.join(format!("audio-{name}.json"));
    let trace_path = directory.join(format!("audio-{name}.trace"));
    let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .arg(directory.join("fixture_audio.exe"))
        .args([
            "--backend",
            backend,
            if native { "--hidden" } else { "--headless" },
            "--audio",
            "--seconds",
            "10",
            "--report",
        ])
        .arg(&report_path)
        .arg("--trace")
        .arg(&trace_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = std::fs::read_to_string(report_path).unwrap();
    assert!(report.contains("\"stop_reason\": \"guest_exit\""));
    assert_eq!(number(&report, "guest_exit_code"), 0.0);
    fn count(report: &str, marker: &str) -> u64 {
        report
            .split(marker)
            .nth(1)
            .unwrap()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap()
    }
    let queued = count(&report, "; queued ");
    let nonzero = count(&report, "; nonzero ");
    assert!(queued > 0 && nonzero > 0 && nonzero <= queued, "{report}");
    let trace = std::fs::read_to_string(trace_path).unwrap();
    for call in [
        "dsound.dll!#1", // The Windows SDK import library uses DirectSoundCreate's ordinal.
        "dsound.dll!IDirectSound8::CreateSoundBuffer",
        "dsound.dll!IDirectSoundBuffer8::Lock",
        "dsound.dll!IDirectSoundBuffer8::Unlock",
        "dsound.dll!IDirectSoundBuffer8::Play",
        "dsound.dll!IDirectSoundBuffer8::GetCurrentPosition",
        "dsound.dll!IDirectSoundBuffer8::Stop",
    ] {
        assert!(trace.contains(call), "missing guest COM call {call}");
    }
    report
}

#[test]
fn guest_pcm_fixture_plays_advances_and_stops_without_a_host_device() {
    for backend in ["jit", "interpreter"] {
        let report = execute_audio(backend, backend, false);
        assert!(report.contains("\"device_requested\": false"));
    }
}

#[test]
#[ignore = "opens the real default sound device with one second of low-amplitude nonzero PCM"]
fn native_audio_output_consumes_non_silent_guest_pcm() {
    let report = execute_audio("native-device", "interpreter", true);
    assert!(report.contains("\"device_requested\": true"));
    assert!(number(&report, "starts") >= 1.0, "{report}");
    assert!(number(&report, "callbacks") > 0.0, "{report}");
    assert!(number(&report, "non_silent_samples") > 0.0, "{report}");
}

#[test]
fn frame_limit_stops_at_presentation_without_reporting_a_guest_exit() {
    let directory = fixtures();
    for backend in ["jit", "interpreter"] {
        let report_path = directory.join(format!("frame-limit-{backend}.json"));
        let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .arg(directory.join("fixture_window.exe"))
            .args([
                "--backend",
                backend,
                "--headless",
                "--frames",
                "1",
                "--seconds",
                "10",
            ])
            .arg("--report")
            .arg(&report_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = std::fs::read_to_string(report_path).unwrap();
        assert!(report.contains("\"stop_reason\": \"frame_limit\""));
        assert!(report.contains("\"guest_exit_code\": null"));
        assert_eq!(number(&report, "frames"), 1.0);
        assert_eq!(number(&report, "input_messages"), 0.0);
        assert!(number(&report, "first_frame_seconds") > 0.0);
    }
}

#[test]
fn hidden_native_frontend_delivers_input_and_waits() {
    let (report, _) = execute("hidden", "interpreter", true);
    assert!(number(&report, "idle_waits") > 0.0);
    assert!(number(&report, "idle_wait_seconds") > 0.0);
}

#[test]
fn host_queue_key_is_translated_to_exactly_one_guest_character() {
    // --self-test-input posts WM_KEYDOWN through PostMessageW. The fixture's
    // guest TranslateMessage must produce exactly one lowercase 'a'; a second
    // host translation would make the fixture fail with guest exit code 23.
    execute("character-once", "interpreter", true);
}

#[test]
fn headless_limit_wakes_an_indefinite_event_wait() {
    let directory = fixtures();
    let report_path = directory.join("wait.json");
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .arg(directory.join("fixture_wait.exe"))
        .args([
            "--backend",
            "interpreter",
            "--headless",
            "--seconds",
            "0.15",
        ])
        .arg("--report")
        .arg(&report_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "indefinite guest wait prevented host time limit"
    );
    let report = std::fs::read_to_string(report_path).unwrap();
    assert!(report.contains("\"stop_reason\": \"time_limit\""));
    assert!(report.contains("\"guest_exit_code\": null"));
    assert!(report.contains("\"first_frame_seconds\": null"));
    assert!(number(&report, "idle_wait_seconds") > 0.05);
}

#[test]
fn no_presentation_does_not_create_a_placeholder_screenshot() {
    let directory = fixtures();
    let report_path = directory.join("no-frame.json");
    let png_path = directory.join("no-frame.png");
    if png_path.exists() {
        std::fs::remove_file(&png_path).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .arg(directory.join("fixture_wait.exe"))
        .args([
            "--backend",
            "interpreter",
            "--headless",
            "--seconds",
            "0.15",
        ])
        .arg("--report")
        .arg(&report_path)
        .arg("--screenshot")
        .arg(&png_path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("guest has not presented a frame"));
    assert!(
        !png_path.exists(),
        "an unpresented initial surface was saved as a guest frame"
    );
    let report = std::fs::read_to_string(report_path).unwrap();
    assert_eq!(number(&report, "frames"), 0.0);
    assert!(report.contains("\"stop_reason\": \"time_limit\""));
    assert!(report.contains("\"screenshot_error\": \"guest has not presented a frame\""));
}

#[test]
fn close_grace_interrupts_a_guest_close_handler_that_never_returns() {
    use std::process::Stdio;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };
    struct Search {
        process: u32,
        window: HWND,
    }
    unsafe extern "system" fn find(hwnd: HWND, argument: LPARAM) -> i32 {
        // SAFETY: EnumWindows synchronously receives a live Search pointer.
        unsafe {
            let search = &mut *(argument as *mut Search);
            let mut process = 0;
            GetWindowThreadProcessId(hwnd, &mut process);
            if process == search.process {
                search.window = hwnd;
                return 0;
            }
        }
        1
    }
    let directory = fixtures();
    let report_path = directory.join("close.json");
    let trace_path = directory.join("close.trace");
    if trace_path.exists() {
        std::fs::remove_file(&trace_path).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .arg(directory.join("fixture_close.exe"))
        .args(["--backend", "interpreter", "--hidden", "--seconds", "6"])
        .arg("--report")
        .arg(&report_path)
        .arg("--trace")
        .arg(&trace_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let mut search = Search {
        process: child.id(),
        window: null_mut(),
    };
    while started.elapsed() < Duration::from_secs(3) {
        // SAFETY: Search is valid throughout the synchronous enumeration.
        unsafe {
            EnumWindows(Some(find), (&mut search as *mut Search) as isize);
        }
        let guest_ready = std::fs::read_to_string(&trace_path)
            .is_ok_and(|trace| trace.contains("user32.dll!CreateWindowExA"));
        if !search.window.is_null() && guest_ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if search.window.is_null()
        || !std::fs::read_to_string(&trace_path)
            .is_ok_and(|trace| trace.contains("user32.dll!CreateWindowExA"))
    {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("host or guest window did not become ready");
    }
    // SAFETY: the HWND belongs to the test's child process, whose lifetime is bounded.
    assert_ne!(unsafe { PostMessageW(search.window, WM_CLOSE, 0, 0) }, 0);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = std::fs::read_to_string(report_path).unwrap();
    assert!(
        report.contains("\"stop_reason\": \"host_window_closed\""),
        "{report}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "close grace did not limit guest execution"
    );
    assert!(report.contains("\"guest_exit_code\": null"));
}

#[test]
fn stdin_control_wakes_idle_guest_and_reuses_capture_and_report_paths() {
    use std::io::{BufRead, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    fn ack(receiver: &mpsc::Receiver<String>) -> String {
        loop {
            let line = receiver
                .recv_timeout(Duration::from_secs(6))
                .expect("control ACK timed out");
            if line.starts_with("control ") {
                return line;
            }
        }
    }
    for (exe, mode) in [
        ("fixture_control.exe", "--headless"),
        ("fixture_control.exe", "--hidden"),
        ("fixture_compute.exe", "--headless"),
        ("fixture_compute.exe", "--hidden"),
        ("fixture_live_input.exe", "--headless"),
        ("fixture_live_input.exe", "--hidden"),
        ("fixture_copied_message.exe", "--headless"),
        ("fixture_copied_message.exe", "--hidden"),
        ("fixture_self_input.exe", "--headless"),
        ("fixture_self_input.exe", "--hidden"),
    ] {
        let directory = fixtures();
        let report_path = directory.join(format!("control-{exe}-{mode}.json"));
        let png_path = directory.join(format!("control-{exe}-{mode} frame.png"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .arg(directory.join(exe))
            .args([
                "--backend",
                "interpreter",
                mode,
                "--control-stdin",
                "--width",
                "64",
                "--height",
                "32",
                "--seconds",
                "10",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut input = child.stdin.take().unwrap();
        let mut ready = false;
        for _ in 0..30 {
            writeln!(input, "report {}", report_path.display()).unwrap();
            input.flush().unwrap();
            assert!(ack(&receiver).starts_with("control ok report"));
            if number(&std::fs::read_to_string(&report_path).unwrap(), "frames") > 0.0 {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready, "control guest did not present a frame");
        // These commands arrive in one stdin batch. Capture/report must run after
        // the click's guest message dispatch, even with an indefinite idle wait.
        writeln!(
            input,
            "reset-metrics\nclick 10 10\ncapture {}\nreport {}",
            png_path.display(),
            report_path.display()
        )
        .unwrap();
        input.flush().unwrap();
        assert!(ack(&receiver).starts_with("control ok reset-metrics start_frame="));
        assert!(ack(&receiver).starts_with("control ok click"));
        assert!(ack(&receiver).starts_with("control ok capture"));
        assert!(ack(&receiver).starts_with("control ok report"));
        let decoder = png::Decoder::new(std::fs::File::open(&png_path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size()];
        reader.next_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[..4], &[0xaa, 0x33, 0x11, 255]);
        let report = std::fs::read_to_string(&report_path).unwrap();
        assert!(report.contains("\"stop_reason\": \"running\""));
        assert!(report.contains("\"warm_active_runtime_between_frame_observations\""));
        assert!(number(&report, "measurement_start_frame") > 0.0);
        assert_eq!(number(&report, "input_messages"), 3.0);
        if exe == "fixture_compute.exe" {
            assert!(number(&report, "runtime_soft_yields") > 0.0);
        } else {
            assert!(
                number(&report, "runtime_steps") < 50.0,
                "control wait busy-polled the guest"
            );
        }
        // Held Ctrl must survive multiple guest timer callbacks, then release.
        writeln!(input, "key-down 17").unwrap();
        input.flush().unwrap();
        assert!(ack(&receiver).starts_with("control ok key-down"));
        std::thread::sleep(Duration::from_millis(50));
        for (command, expected) in [
            (
                format!("capture {}", png_path.display()),
                [0x33, 0xaa, 0x11, 255],
            ),
            (
                format!("key-up 17\ncapture {}", png_path.display()),
                [0x11, 0x33, 0xaa, 255],
            ),
        ] {
            writeln!(input, "{command}").unwrap();
            input.flush().unwrap();
            if command.starts_with("key-up") {
                assert!(ack(&receiver).starts_with("control ok key-up"));
            }
            assert!(ack(&receiver).starts_with("control ok capture"));
            let decoder = png::Decoder::new(std::fs::File::open(&png_path).unwrap());
            let mut reader = decoder.read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size()];
            reader.next_frame(&mut pixels).unwrap();
            assert_eq!(&pixels[..4], &expected, "{exe} {mode} {command}");
        }
        writeln!(input, "key 256\nkey 0x41\nclose").unwrap();
        input.flush().unwrap();
        assert!(ack(&receiver).starts_with("control error"));
        assert!(ack(&receiver).starts_with("control ok key"));
        assert!(ack(&receiver).starts_with("control ok close"));
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn failed_input_without_a_window_does_not_block_diagnostic_report_or_close() {
    use std::io::{BufRead, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    let directory = fixtures();
    let path = directory.join("control-no-window.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .arg(directory.join("fixture_wait.exe"))
        .args([
            "--backend",
            "interpreter",
            "--headless",
            "--control-stdin",
            "--seconds",
            "5",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            if let Ok(line) = line
                && line.starts_with("control ")
                && sender.send(line).is_err()
            {
                break;
            }
        }
    });
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "key-up 17\nreport {}\nclose", path.display()).unwrap();
    input.flush().unwrap();
    let first = receiver.recv_timeout(Duration::from_secs(6)).unwrap();
    assert!(
        first.starts_with("control error key has no guest target window"),
        "{first}"
    );
    let report = receiver.recv_timeout(Duration::from_secs(6)).unwrap();
    assert!(report.starts_with("control ok report"), "{report}");
    let close = receiver.recv_timeout(Duration::from_secs(6)).unwrap();
    assert!(close.starts_with("control ok close"), "{close}");
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(number(&text, "input_messages"), 0.0);
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn host_deadline_stops_a_continuously_ready_compute_context() {
    let directory = fixtures();
    let report_path = directory.join("compute-deadline.json");
    let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .arg(directory.join("fixture_compute.exe"))
        .args([
            "--headless",
            "--backend",
            "interpreter",
            "--seconds",
            "0.1",
            "--report",
        ])
        .arg(&report_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = std::fs::read_to_string(report_path).unwrap();
    assert!(report.contains("\"stop_reason\": \"time_limit\""));
    assert!(report.contains("\"guest_exit_code\": null"));
    assert!(report.contains("\"failure\": null"));
    assert!(number(&report, "runtime_soft_yields") > 0.0);
    assert_eq!(number(&report, "guest_contexts"), 2.0);
}

#[test]
fn sliced_jit_and_interpreter_compute_exactly_the_unsliced_guest_results() {
    let directory = fixtures();
    for (backend, jit) in [("jit", true), ("interpreter", false)] {
        let mut reference = adv_runtime::boot::Runtime::new(
            directory.join("fixture_slice.exe"),
            directory.join(format!("slice-reference-data-{backend}")),
            jit,
            64,
            32,
        )
        .unwrap();
        // The fixture verifies each of two workers performed exactly 2,000,000
        // volatile increments and computed the same final register-derived result.
        assert!(matches!(
            reference.step(),
            adv_machine::RunResult::Exited(0)
        ));
        let report_path = directory.join(format!("slice-{backend}.json"));
        let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .arg(directory.join("fixture_slice.exe"))
            .args([
                "--backend",
                backend,
                "--headless",
                "--seconds",
                "60",
                "--report",
            ])
            .arg(&report_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{backend}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let report = std::fs::read_to_string(report_path).unwrap();
        assert!(report.contains("\"stop_reason\": \"guest_exit\""));
        assert_eq!(number(&report, "guest_exit_code"), 0.0);
        assert_eq!(number(&report, "guest_contexts"), 3.0);
        assert!(number(&report, "runtime_soft_yields") > 0.0, "{report}");
    }
}
