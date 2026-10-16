//! On-device JIT extension and external StikDebug requests (IOS-JIT-2).
use objc2::{
    MainThreadMarker, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSExtensionItem, NSString, NSURL,
};
use std::{
    cell::RefCell,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

#[derive(Default)]
struct RequestState {
    waiting: bool,
    /// What the running request does: a `Task` name or `external`.
    task: Option<&'static str>,
    stage: Option<String>,
    error: Option<String>,
}
static REQUEST: Mutex<RequestState> = Mutex::new(RequestState {
    waiting: false,
    task: None,
    stage: None,
    error: None,
});
/// Whether the helper holds the DDI set; unknown until it is asked.
static DDI: Mutex<Option<bool>> = Mutex::new(None);

/// What one helper request does; the name is the helper's `mode`.
#[derive(Clone, Copy, PartialEq)]
enum Task {
    Enable,
    DdiStatus,
    DownloadDdi,
}

impl Task {
    fn name(self) -> &'static str {
        match self {
            Task::Enable => "enable",
            Task::DdiStatus => "ddiStatus",
            Task::DownloadDdi => "downloadDdi",
        }
    }
}
thread_local! {
    static EXTENSION: RefCell<Option<(Retained<AnyObject>, Retained<AnyObject>)>> = const { RefCell::new(None) };
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JitStatus {
    ready: bool,
    error: Option<String>,
    waiting: bool,
    task: Option<&'static str>,
    stage: Option<String>,
    live_container: bool,
    pairing_file: bool,
    /// Whether the developer disk image is downloaded; `None` until checked.
    ddi: Option<bool>,
}

fn pairing_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .document_dir()
        .map(|path| path.join("pairingFile.plist"))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn jit_status(app: AppHandle) -> JitStatus {
    let state = adv_shell::code_pool_state();
    let request = REQUEST.lock().unwrap();
    JitStatus {
        ready: matches!(state, Some(Ok(()))),
        error: request
            .error
            .clone()
            .or_else(|| state.and_then(Result::err)),
        waiting: request.waiting,
        task: request.task,
        stage: request.stage.clone(),
        live_container: std::env::var_os("LC_HOME_PATH").is_some(),
        pairing_file: pairing_path(&app).is_ok_and(|path| path.is_file()),
        ddi: *DDI.lock().unwrap(),
    }
}

#[tauri::command]
pub fn request_jit(app: AppHandle, external: Option<bool>) -> Result<(), String> {
    if let Some(result) = adv_shell::code_pool_state() {
        return result;
    }
    if !external.unwrap_or(false) {
        if std::env::var_os("LC_HOME_PATH").is_some() {
            return Err("Use StikDebug inside LiveContainer".into());
        }
        let pairing =
            std::fs::read(pairing_path(&app)?).map_err(|e| format!("pairingFile.plist: {e}"))?;
        return run_helper(app, Task::Enable, pairing);
    }
    let mut state = REQUEST.lock().unwrap();
    if state.waiting {
        return Ok(());
    }
    *state = RequestState {
        waiting: true,
        task: Some("external"),
        stage: Some("external".into()),
        error: None,
    };
    drop(state);
    let handle = app.clone();
    let result = app
        .run_on_main_thread(move || open_stikdebug(handle))
        .map_err(|e| e.to_string());
    if let Err(error) = &result {
        finish(&app, Err(error.clone()));
    }
    result
}

/// Fixture: `ADV_IOS_JIT_AT_LAUNCH` names one helper task to run at launch,
/// so device tests need no tap.
pub fn request_at_launch(app: &AppHandle) {
    let Ok(name) = std::env::var("ADV_IOS_JIT_AT_LAUNCH") else {
        return;
    };
    let result = match name.as_str() {
        "enable" => request_jit(app.clone(), Some(false)),
        "ddiStatus" => check_ddi(app.clone()),
        "downloadDdi" => download_ddi(app.clone()),
        _ => Err(format!("unknown ADV_IOS_JIT_AT_LAUNCH task {name}")),
    };
    if let Err(error) = result {
        log::warn!("JIT request at launch: {error}");
    }
}

/// Asks the helper whether the developer disk image is downloaded.
#[tauri::command]
pub fn check_ddi(app: AppHandle) -> Result<(), String> {
    run_helper(app, Task::DdiStatus, Vec::new())
}

/// Downloads the developer disk image ahead of enabling JIT, while the
/// user's usual network, proxy included, still holds the VPN slot.
#[tauri::command]
pub fn download_ddi(app: AppHandle) -> Result<(), String> {
    run_helper(app, Task::DownloadDdi, Vec::new())
}

fn run_helper(app: AppHandle, task: Task, pairing: Vec<u8>) -> Result<(), String> {
    let mut state = REQUEST.lock().unwrap();
    if state.waiting {
        return Ok(());
    }
    *state = RequestState {
        waiting: true,
        task: Some(task.name()),
        stage: Some("starting".into()),
        error: None,
    };
    drop(state);
    let handle = app.clone();
    let result = app
        .run_on_main_thread(move || {
            if let Err(error) = start_helper(&handle, task, pairing) {
                finish(&handle, Err(error));
            }
        })
        .map_err(|e| e.to_string());
    if let Err(error) = &result {
        finish(&app, Err(error.clone()));
    }
    result
}

/// Copies a pairing file chosen in Files to where the helper reads it
/// (IOS-JIT-2). Returns whether one was imported; the errors are codes the
/// settings row explains.
#[tauri::command]
pub async fn import_pairing_file(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_storage::StorageExt;
    let Some(source) = app.storage().pick_file().await?.path else {
        return Ok(false);
    };
    let bytes = std::fs::read(&source);
    let _ = std::fs::remove_file(&source);
    let bytes = bytes.map_err(|e| e.to_string())?;
    check_pairing(&bytes)?;
    let dest = pairing_path(&app)?;
    let staging = dest.with_extension("plist.part");
    std::fs::write(&staging, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&staging, &dest).map_err(|e| e.to_string())?;
    Ok(true)
}

/// StikDebug's RemotePairing file, not SideStore's lockdown pairing record,
/// which shares the plist format but cannot open the tunnel.
fn check_pairing(bytes: &[u8]) -> Result<(), String> {
    let dict: plist::Dictionary =
        plist::from_bytes(bytes).map_err(|_| "invalidPairing".to_string())?;
    if ["public_key", "private_key", "identifier"]
        .iter()
        .all(|key| dict.contains_key(key))
    {
        Ok(())
    } else if dict.contains_key("HostPrivateKey") {
        Err("lockdownPairing".into())
    } else {
        Err("invalidPairing".into())
    }
}

fn finish(app: &AppHandle, result: Result<(), String>) {
    let error = result.err();
    match &error {
        Some(error) => log::warn!("JIT request: {error}"),
        None => log::info!("JIT request finished"),
    }
    // Cancel the completed extension request on its owning thread.
    let _ = app.run_on_main_thread(move || {
        EXTENSION.with(|slot| {
            if let Some((extension, request)) = slot.borrow_mut().take() {
                // A missing private selector would raise an Objective-C
                // exception, which aborts once it reaches Rust frames.
                let cancel = objc2::sel!(cancelExtensionRequestWithIdentifier:);
                // SAFETY: request is the identifier returned by this NSExtension.
                unsafe {
                    let responds: bool = msg_send![&*extension, respondsToSelector: cancel];
                    if responds {
                        let _: () =
                            msg_send![&*extension, cancelExtensionRequestWithIdentifier: &*request];
                    } else {
                        log::warn!("JIT extension request cannot be cancelled");
                    }
                }
            }
        });
        *REQUEST.lock().unwrap() = RequestState {
            waiting: false,
            task: None,
            stage: None,
            error,
        };
    });
}

fn start_helper(app: &AppHandle, task: Task, pairing: Vec<u8>) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let endpoint = NSString::from_str(&format!("{port}:{token}"));
    // Read the embedded bundle's actual identifier; sideloaders rewrite both IDs.
    let plugins = NSBundle::mainBundle()
        .builtInPlugInsPath()
        .ok_or("JIT extension is missing")?;
    let path = NSString::from_str(&format!("{plugins}/HitodeJIT.appex"));
    let bundle = NSBundle::bundleWithPath(&path).ok_or("JIT extension is missing")?;
    let identifier = bundle
        .bundleIdentifier()
        .ok_or("JIT extension has no bundle identifier")?;
    let class = AnyClass::get(c"NSExtension").ok_or("NSExtension is unavailable")?;
    let item = NSExtensionItem::new();
    let info = NSDictionary::from_slices(&[&*NSString::from_str("endpoint")], &[&*endpoint]);
    // SAFETY: Foundation dictionary contains only strings, supported by NSSecureCoding.
    unsafe {
        item.setUserInfo(Some(&info.cast_unchecked()));
    }
    let items = NSArray::from_slice(&[&*item]);
    // SAFETY: private Foundation methods use the Objective-C signatures shown here.
    let (extension, request) = unsafe {
        let mut error: Option<Retained<NSError>> = None;
        let extension: Option<Retained<AnyObject>> = msg_send![class,
            extensionWithIdentifier: &*identifier, excludingDisabledExtensions: false, error: &mut error];
        let extension = extension.ok_or_else(|| {
            error.as_ref().map_or_else(
                || "Cannot create JIT extension".into(),
                |e| e.localizedDescription().to_string(),
            )
        })?;
        let request: Option<Retained<AnyObject>> = msg_send![&*extension,
            beginExtensionRequestWithInputItems: &*items, error: &mut error];
        let request = request.ok_or_else(|| {
            error.as_ref().map_or_else(
                || "Cannot start JIT extension".into(),
                |e| e.localizedDescription().to_string(),
            )
        })?;
        (extension, request)
    };
    EXTENSION.with(|slot| *slot.borrow_mut() = Some((extension, request)));
    let app = app.clone();
    std::thread::spawn(move || {
        let result = listen(listener, &token, task, pairing);
        finish(&app, result);
    });
    Ok(())
}

#[derive(serde::Deserialize)]
struct Progress {
    stage: String,
    error: Option<String>,
}

fn listen(listener: TcpListener, token: &str, task: Task, pairing: Vec<u8>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let (stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("JIT extension did not connect: {e}")),
        }
    };
    // Darwin sockets inherit O_NONBLOCK from the listener they were accepted on.
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .by_ref()
        .take(128)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.trim_end() != token {
        return Err("JIT extension authentication failed".into());
    }
    let request = serde_json::json!({
        "mode": task.name(),
        "pid": std::process::id(),
        "pairing": pairing,
    });
    writeln!(reader.get_mut(), "{request}").map_err(|e| e.to_string())?;
    reader
        .get_ref()
        // Above the helper's 240 s enable limit and its 300 s per-file download.
        .set_read_timeout(Some(Duration::from_secs(330)))
        .map_err(|e| e.to_string())?;
    let mut preparation = None;
    let result = (|| {
        loop {
            line.clear();
            if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("JIT extension disconnected before completing".into());
            }
            let progress: Progress = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            log::info!("JIT helper: {}", progress.stage);
            match progress.stage.as_str() {
                "finished" => {
                    if task == Task::DownloadDdi && progress.error.is_none() {
                        *DDI.lock().unwrap() = Some(true);
                    }
                    return progress.error.map_or(Ok(()), Err);
                }
                "ddiPresent" => *DDI.lock().unwrap() = Some(true),
                "ddiMissing" => *DDI.lock().unwrap() = Some(false),
                _ => {}
            }
            if progress.stage == "attaching" && preparation.is_none() {
                preparation = Some(std::thread::spawn(|| {
                    adv_shell::prepare_code_pool_after_attach(Duration::from_secs(30))
                }));
            }
            REQUEST.lock().unwrap().stage = Some(progress.stage);
        }
    })();
    if task != Task::Enable {
        return result;
    }
    if let Some(thread) = preparation {
        let prepared = thread
            .join()
            .map_err(|_| "JIT preparation thread failed".to_string())?;
        result?;
        prepared
    } else {
        result.and(Err("JIT extension did not attach".into()))
    }
}

fn open_stikdebug(app: AppHandle) {
    let bundle = NSBundle::mainBundle()
        .bundleIdentifier()
        .map_or_else(|| app.config().identifier.clone(), |id| id.to_string());
    let url = NSURL::URLWithString(&NSString::from_str(&format!(
        "stikdebug://enable-jit?bundle-id={bundle}&pid={}&script-name=universal.js",
        std::process::id(),
    )))
    .unwrap();
    let completion = block2::RcBlock::new(move |opened: objc2::runtime::Bool| {
        let handle = app.clone();
        if !opened.as_bool() {
            finish(
                &handle,
                Err("Cannot open StikDebug; install it before requesting external JIT".into()),
            );
            return;
        }
        std::thread::spawn(move || {
            let result = adv_shell::prepare_code_pool_after_attach(Duration::from_secs(120));
            finish(&handle, result);
        });
    });
    // SAFETY: invoked on the main thread, URL and options have their declared types.
    unsafe {
        objc2_ui_kit::UIApplication::sharedApplication(MainThreadMarker::new().unwrap())
            .openURL_options_completionHandler(&url, &NSDictionary::new(), Some(&completion));
    }
}
