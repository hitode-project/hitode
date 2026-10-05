//! Opt-in native address map for an external sampler. This records finalized
//! bodies, not execution counts or publication/retirement guarantees. It never
//! changes compilation policy, links, or generated instructions. Trampolines
//! are not included. Reused address ranges require timestamp-aware attribution.
use crate::Region;
use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

pub(super) fn record_body(
    region: &Region,
    optimization: &str,
    base: usize,
    size: usize,
    instructions: usize,
) {
    static MAP: OnceLock<Option<Mutex<Option<File>>>> = OnceLock::new();
    let Some(map) = MAP.get_or_init(|| {
        let directory = std::env::var_os("ADV_JIT_MAP_DIR")?;
        let directory = std::path::PathBuf::from(directory);
        match std::fs::create_dir_all(&directory)
            .and_then(|_| File::create(directory.join(format!("jit-{}.tsv", std::process::id()))))
        {
            Ok(file) => Some(Mutex::new(Some(file))),
            Err(error) => {
                eprintln!("warning: JIT address map disabled: {error}");
                None
            }
        }
    }) else {
        return;
    };
    let mut sink = map.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(file) = sink.as_mut() else {
        return;
    };
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Decimal TSV: Unix ns, native base, body bytes, guest start, optimization,
    // guest address span bytes, decoded instructions. One row per compilation.
    if let Err(error) = writeln!(
        file,
        "{ns}\t{base}\t{size}\t{}\t{optimization}\t{}\t{instructions}",
        region.start, region.len,
    ) {
        eprintln!("warning: JIT address map disabled after write failure: {error}");
        *sink = None;
    }
}
