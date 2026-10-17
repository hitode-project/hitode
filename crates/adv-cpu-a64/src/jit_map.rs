//! Opt-in native address maps for samplers. They record placed units, not
//! execution counts, and never change translation.
//!
//! `ADV_JIT_MAP_DIR` writes the x64 backend's `jit-{pid}.tsv` rows for an
//! external sampler. [`enable_memory`] keeps the same rows in process for an
//! in-process sampler. A cleared cache reuses addresses, so attribution takes
//! the newest unit placed before the sample.
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static MEMORY: AtomicBool = AtomicBool::new(false);

/// Units in placement order. The cache appends at rising addresses until it
/// is cleared, so each generation is sorted by base.
#[derive(Default)]
struct Generations(Vec<Generation>);

struct Generation {
    since_ns: u128,
    /// (base, end, guest start)
    units: Vec<(usize, usize, u32)>,
}

static UNITS: Mutex<Generations> = Mutex::new(Generations(Vec::new()));

/// Starts keeping units in process for [`lookup`]. Units placed earlier stay unknown.
pub fn enable_memory() {
    MEMORY.store(true, Ordering::Release);
}

/// The guest start of the unit that held host `pc` at Unix time `at_ns`.
pub fn lookup(pc: usize, at_ns: u128) -> Option<u32> {
    let units = UNITS.lock().unwrap_or_else(|poison| poison.into_inner());
    // Newer generations overwrite older ones from the bottom of the cache.
    for generation in units.0.iter().rev() {
        if generation.since_ns > at_ns {
            continue;
        }
        let at = generation.units.partition_point(|&(base, ..)| base <= pc);
        if let Some(&(_, end, eip)) = at.checked_sub(1).map(|at| &generation.units[at])
            && pc < end
        {
            return Some(eip);
        }
    }
    None
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Records a unit of `size` bytes at `base` translated from `eip`.
pub(crate) fn record(base: usize, size: usize, eip: u32, retired: u32) {
    if MEMORY.load(Ordering::Acquire) {
        let mut units = UNITS.lock().unwrap_or_else(|poison| poison.into_inner());
        let unit = (base, base + size, eip);
        match units.0.last_mut() {
            Some(last) if last.units.last().is_none_or(|&(_, end, _)| end <= base) => {
                last.units.push(unit)
            }
            _ => units.0.push(Generation {
                since_ns: now_ns(),
                units: vec![unit],
            }),
        }
    }
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
    // Decimal TSV as in x64: Unix ns, native base, bytes, guest start, mode,
    // guest span (unknown here, 0), retired guest instructions.
    let row = format!("{}\t{base}\t{size}\t{eip}\ta64\t0\t{retired}\n", now_ns());
    let mut file = file.lock().unwrap_or_else(|poison| poison.into_inner());
    let _ = file.write_all(row.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both cases share the process-wide map, so they run as one test.
    #[test]
    fn lookup_takes_the_newest_unit_placed_before_the_sample() {
        enable_memory();
        record(0x1000, 0x40, 0x401000, 3);
        record(0x1040, 0x20, 0x401100, 2);
        let first = now_ns();
        assert_eq!(lookup(0x1010, first), Some(0x401000));
        assert_eq!(lookup(0x1050, first), Some(0x401100));
        assert_eq!(lookup(0x1060, first), None);
        // A cleared cache places again from its bottom.
        std::thread::sleep(std::time::Duration::from_millis(2));
        record(0x1000, 0x30, 0x402000, 1);
        let second = now_ns();
        assert_eq!(lookup(0x1010, second), Some(0x402000));
        assert_eq!(lookup(0x1010, first), Some(0x401000));
        // Beyond the new unit, the older generation still answers.
        assert_eq!(lookup(0x1050, second), Some(0x401100));
    }
}
