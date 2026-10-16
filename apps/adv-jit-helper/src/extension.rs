//! Foundation extension entry and authenticated loopback progress channel.
use objc2::define_class;
use objc2_foundation::{
    NSExtensionContext, NSExtensionItem, NSExtensionRequestHandling, NSObject, NSObjectProtocol,
    NSString,
};
use serde::Deserialize;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::PathBuf,
    time::Duration,
};

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
enum Mode {
    #[default]
    Enable,
    /// Reports `ddiPresent` or `ddiMissing` without touching the network.
    DdiStatus,
    /// Fetches the DDI set without the tunnel.
    DownloadDdi,
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    pairing: Vec<u8>,
}

define_class!(
    // NSExtensionPrincipalClass names this class; objc2's default name would
    // carry the module path and crate version.
    #[unsafe(super(NSObject))]
    #[name = "HitodeJitRequestHandler"]
    pub struct HitodeJitRequestHandler;
    unsafe impl NSObjectProtocol for HitodeJitRequestHandler {}
    unsafe impl NSExtensionRequestHandling for HitodeJitRequestHandler {
        #[unsafe(method(beginRequestWithExtensionContext:))]
        fn begin(&self, context: &NSExtensionContext) {
            let items = context.inputItems();
            let endpoint = items
                .firstObject()
                .and_then(|item| {
                    item.downcast_ref::<NSExtensionItem>()
                        .and_then(|item| item.userInfo())
                })
                .and_then(|info| info.objectForKey(&NSString::from_str("endpoint")))
                .and_then(|value| value.downcast_ref::<NSString>().map(ToString::to_string));
            if let Some(endpoint) = endpoint {
                std::thread::spawn(move || {
                    let _ = run(&endpoint);
                });
            }
            // The host retains the request and cancels it after reading the final result.
        }
    }
);

fn run(endpoint: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (port, token) = endpoint.split_once(':').ok_or("missing helper endpoint")?;
    let port: u16 = port.parse()?;
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "{token}")?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line)?;
    // Application Support, not Caches: the system may purge caches, and a
    // fresh download may be impossible while LocalDevVPN holds the VPN slot.
    let cache = PathBuf::from(std::env::var("HOME")?).join("Library/Application Support/HitodeJIT");
    let output = std::sync::Mutex::new(stream);
    let send = |stage: &str, error: Option<String>| {
        let message = serde_json::json!({"stage": stage, "error": error});
        let _ = writeln!(output.lock().unwrap(), "{message}");
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let cryptex = crate::device::local_cryptex();
    let result = match request.mode {
        Mode::DdiStatus => {
            let present = crate::device::ddi_present(&cache, cryptex);
            send(if present { "ddiPresent" } else { "ddiMissing" }, None);
            Ok(())
        }
        Mode::DownloadDdi => runtime.block_on(async {
            crate::device::download_ddi(&cache, cryptex, &|stage| send(stage, None))
                .await
                .map(drop)
                .map_err(|error| error.to_string())
        }),
        Mode::Enable => runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(240),
                crate::device::enable(request.pid, &request.pairing, &cache, &|stage| {
                    send(stage, None)
                }),
            )
            .await
            .map_err(|_| "JIT helper timed out".to_string())?
            .map_err(|error| error.to_string())
        }),
    };
    send("finished", result.err());
    Ok(())
}
