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
    stage: Option<String>,
    error: Option<String>,
}
static REQUEST: Mutex<RequestState> = Mutex::new(RequestState {
    waiting: false,
    stage: None,
    error: None,
});
thread_local! {
    static EXTENSION: RefCell<Option<(Retained<AnyObject>, Retained<AnyObject>)>> = const { RefCell::new(None) };
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JitStatus {
    ready: bool,
    error: Option<String>,
    waiting: bool,
    stage: Option<String>,
    live_container: bool,
    pairing_file: bool,
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
        stage: request.stage.clone(),
        live_container: std::env::var_os("LC_HOME_PATH").is_some(),
        pairing_file: pairing_path(&app).is_ok_and(|path| path.is_file()),
    }
}

#[tauri::command]
pub fn request_jit(app: AppHandle, external: Option<bool>) -> Result<(), String> {
    if let Some(result) = adv_shell::code_pool_state() {
        return result;
    }
    let mut state = REQUEST.lock().unwrap();
    if state.waiting {
        return Ok(());
    }
    let external = external.unwrap_or(false);
    let pairing = if external {
        Vec::new()
    } else {
        if std::env::var_os("LC_HOME_PATH").is_some() {
            return Err("Use StikDebug inside LiveContainer".into());
        }
        std::fs::read(pairing_path(&app)?).map_err(|e| format!("pairingFile.plist: {e}"))?
    };
    *state = RequestState {
        waiting: true,
        stage: Some(if external { "external" } else { "starting" }.into()),
        error: None,
    };
    drop(state);
    let handle = app.clone();
    let result = app
        .run_on_main_thread(move || {
            if external {
                open_stikdebug(handle);
            } else if let Err(error) = start_helper(&handle, pairing) {
                finish(&handle, Err(error));
            }
        })
        .map_err(|e| e.to_string());
    if let Err(error) = &result {
        finish(&app, Err(error.clone()));
    }
    result
}

fn finish(app: &AppHandle, result: Result<(), String>) {
    let error = result.err();
    if let Some(error) = &error {
        log::warn!("JIT request: {error}");
    }
    // Cancel the completed extension request on its owning thread.
    let _ = app.run_on_main_thread(move || {
        EXTENSION.with(|slot| {
            if let Some((extension, request)) = slot.borrow_mut().take() {
                // SAFETY: request is the identifier returned by this NSExtension.
                unsafe {
                    let _: () = msg_send![&*extension, cancelRequestWithIdentifier: &*request];
                }
            }
        });
        *REQUEST.lock().unwrap() = RequestState {
            waiting: false,
            stage: None,
            error,
        };
    });
}

fn start_helper(app: &AppHandle, pairing: Vec<u8>) -> Result<(), String> {
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
        let result = listen(listener, &token, pairing);
        finish(&app, result);
    });
    Ok(())
}

#[derive(serde::Deserialize)]
struct Progress {
    stage: String,
    error: Option<String>,
}

fn listen(listener: TcpListener, token: &str, pairing: Vec<u8>) -> Result<(), String> {
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
    let request = serde_json::json!({"pid": std::process::id(), "pairing": pairing});
    writeln!(reader.get_mut(), "{request}").map_err(|e| e.to_string())?;
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(250)))
        .map_err(|e| e.to_string())?;
    let mut preparation = None;
    let result = (|| {
        loop {
            line.clear();
            if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("JIT extension disconnected before completing".into());
            }
            let progress: Progress = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if progress.stage == "finished" {
                return progress.error.map_or(Ok(()), Err);
            }
            if progress.stage == "attaching" && preparation.is_none() {
                preparation = Some(std::thread::spawn(|| {
                    adv_shell::prepare_code_pool_after_attach(Duration::from_secs(30))
                }));
            }
            REQUEST.lock().unwrap().stage = Some(progress.stage);
        }
    })();
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
