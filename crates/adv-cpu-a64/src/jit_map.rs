//! Opt-in native address maps for samplers. They record placed units, not
//! execution counts, and never change translation.
//!
//! `ADV_JIT_MAP_DIR` writes the x64 backend's `jit-{pid}.tsv` rows for an
//! external sampler. [`enable_memory`] keeps the same rows in process for an
//! in-process sampler, with the guest instruction and [`Part`] of every
//! stretch of a unit and the names of the shared stubs. A cleared cache
//! reuses addresses, so attribution takes the newest unit placed before the
//! sample.
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static MEMORY: AtomicBool = AtomicBool::new(false);

/// What a stretch of a unit's code does for its guest instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    /// The instruction's inline code.
    Body,
    /// Its out-of-line paths: exits, misses, helper calls.
    Cold,
    /// The checks that open an x87 run (A64F-FP-7).
    X87Entry,
    /// An x87 run writing its state back to `CpuState`.
    X87Publish,
}

/// Where a stretch starts: byte offset in the unit, guest instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub offset: u32,
    pub eip: u32,
    pub part: Part,
}

/// Where a host address is in translated code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    /// Guest start of the unit.
    pub unit: u32,
    /// The guest instruction and part, when the unit recorded marks.
    pub eip: Option<u32>,
    pub part: Part,
}

/// Units in placement order. The cache appends at rising addresses until it
/// is cleared, so each generation is sorted by base.
#[derive(Default)]
struct Generations(Vec<Generation>);

struct Generation {
    since_ns: u128,
    units: Vec<Unit>,
}

struct Unit {
    base: usize,
    end: usize,
    eip: u32,
    /// By rising offset.
    marks: Box<[Mark]>,
}

static UNITS: Mutex<Generations> = Mutex::new(Generations(Vec::new()));

/// Shared stubs: (base, end, name).
static STUBS: Mutex<Vec<(usize, usize, &'static str)>> = Mutex::new(Vec::new());

/// Starts keeping units in process for [`lookup`]. Units placed earlier stay unknown.
pub fn enable_memory() {
    MEMORY.store(true, Ordering::Release);
}

/// The guest start of the unit that held host `pc` at Unix time `at_ns`.
pub fn lookup(pc: usize, at_ns: u128) -> Option<u32> {
    locate(pc, at_ns).map(|location| location.unit)
}

/// Where host `pc` was in translated code at Unix time `at_ns`.
pub fn locate(pc: usize, at_ns: u128) -> Option<Location> {
    let units = UNITS.lock().unwrap_or_else(|poison| poison.into_inner());
    // Newer generations overwrite older ones from the bottom of the cache.
    for generation in units.0.iter().rev() {
        if generation.since_ns > at_ns {
            continue;
        }
        let at = generation.units.partition_point(|unit| unit.base <= pc);
        if let Some(unit) = at.checked_sub(1).map(|at| &generation.units[at])
            && pc < unit.end
        {
            let offset = (pc - unit.base) as u32;
            let mark = unit.marks.partition_point(|mark| mark.offset <= offset);
            let mark = mark.checked_sub(1).map(|at| unit.marks[at]);
            return Some(Location {
                unit: unit.eip,
                eip: mark.map(|mark| mark.eip),
                part: mark.map_or(Part::Body, |mark| mark.part),
            });
        }
    }
    None
}

/// The shared stub holding host `pc`.
pub fn stub(pc: usize) -> Option<&'static str> {
    let stubs = STUBS.lock().unwrap_or_else(|poison| poison.into_inner());
    stubs
        .iter()
        .find(|&&(base, end, _)| (base..end).contains(&pc))
        .map(|&(.., name)| name)
}

/// Records the shared stubs at `base`, each running to the next or `end`.
pub(crate) fn record_stubs(base: usize, end: usize, offsets: &[(usize, &'static str)]) {
    if !MEMORY.load(Ordering::Acquire) {
        return;
    }
    let mut sorted = offsets.to_vec();
    sorted.sort_unstable_by_key(|&(offset, _)| offset);
    let mut stubs = STUBS.lock().unwrap_or_else(|poison| poison.into_inner());
    for (n, &(offset, name)) in sorted.iter().enumerate() {
        let next = sorted.get(n + 1).map_or(end, |&(next, _)| base + next);
        stubs.push((base + offset, next, name));
    }
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Records a unit of `size` bytes at `base` translated from `eip`, whose
/// stretches start at `marks`.
pub(crate) fn record(base: usize, size: usize, eip: u32, retired: u32, marks: &[Mark]) {
    if MEMORY.load(Ordering::Acquire) {
        let mut units = UNITS.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut marks = marks.to_vec();
        marks.sort_by_key(|mark| mark.offset);
        let unit = Unit {
            base,
            end: base + size,
            eip,
            marks: marks.into(),
        };
        match units.0.last_mut() {
            Some(last) if last.units.last().is_none_or(|last| last.end <= base) => {
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

/// The base and marks of every unit recorded for `eip`.
#[cfg(test)]
pub(crate) fn units_of(eip: u32) -> Vec<(usize, Vec<Mark>)> {
    let units = UNITS.lock().unwrap_or_else(|poison| poison.into_inner());
    units
        .0
        .iter()
        .flat_map(|generation| &generation.units)
        .filter(|unit| unit.eip == eip)
        .map(|unit| (unit.base, unit.marks.to_vec()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both cases share the process-wide map, so they run as one test.
    #[test]
    fn lookup_takes_the_newest_unit_placed_before_the_sample() {
        enable_memory();
        let mark = |offset, eip, part| Mark { offset, eip, part };
        record(
            0x1000,
            0x40,
            0x401000,
            3,
            &[
                mark(0x20, 0x401004, Part::Cold),
                mark(0, 0x401000, Part::Body),
            ],
        );
        record(0x1040, 0x20, 0x401100, 2, &[]);
        let first = now_ns();
        assert_eq!(lookup(0x1010, first), Some(0x401000));
        assert_eq!(lookup(0x1050, first), Some(0x401100));
        assert_eq!(lookup(0x1060, first), None);
        let location = |unit, eip, part| Some(Location { unit, eip, part });
        assert_eq!(
            locate(0x101c, first),
            location(0x401000, Some(0x401000), Part::Body)
        );
        assert_eq!(
            locate(0x1024, first),
            location(0x401000, Some(0x401004), Part::Cold)
        );
        assert_eq!(locate(0x1044, first), location(0x401100, None, Part::Body));
        // A cleared cache places again from its bottom.
        std::thread::sleep(std::time::Duration::from_millis(2));
        record(0x1000, 0x30, 0x402000, 1, &[]);
        let second = now_ns();
        assert_eq!(lookup(0x1010, second), Some(0x402000));
        assert_eq!(lookup(0x1010, first), Some(0x401000));
        // Beyond the new unit, the older generation still answers.
        assert_eq!(lookup(0x1050, second), Some(0x401100));
        record_stubs(0x9000, 0x9100, &[(0x40, "exit"), (0, "enter")]);
        assert_eq!(stub(0x9010), Some("enter"));
        assert_eq!(stub(0x9050), Some("exit"));
        assert_eq!(stub(0x9100), None);
    }
}
