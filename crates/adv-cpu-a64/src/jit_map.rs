//! Opt-in native address map for an external sampler, in the x64 backend's
//! `jit-{pid}.tsv` format under `ADV_JIT_MAP_DIR`. It records placed units,
//! not execution counts, and never changes translation. A cleared cache reuses
//! addresses, so attribution takes the newest row placed before a sample.
use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

/// Records a unit of `size` bytes at `base` translated from `eip`.
pub(crate) fn record(base: usize, size: usize, eip: u32, retired: u32) {
    static MAP: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    let Some(file) = MAP.get_or_init(|| {
        let directory = std::path::PathBuf::from(std::env::var_os("ADV_JIT_MAP_DIR")?);
        let path = directory.join(format!("jit-{}.tsv", std::process::id()));
        match std::fs::create_dir_all(&directory).and_then(|_| File::create(path)) {
            Ok(file) => Some(Mutex::new(file)),
            Err(error) => {
                eprintln!("warning: JIT address map disabled: {error}");
                None
            }
        }
    }) else {
        return;
    };
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Decimal TSV as in x64: Unix ns, native base, bytes, guest start, mode,
    // guest span (unknown here, 0), retired guest instructions.
    let row = format!("{ns}\t{base}\t{size}\t{eip}\ta64\t0\t{retired}\n");
    let mut file = file.lock().unwrap_or_else(|poison| poison.into_inner());
    let _ = file.write_all(row.as_bytes());
}
