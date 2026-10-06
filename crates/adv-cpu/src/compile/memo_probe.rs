//! Measurement aid for result reuse: `ADV_MEMO_PROBE=pc,pc,...` (hexadecimal
//! guest PCs) with `ADV_MEMO_PROBE_OUT=path` logs the entry state of every
//! execution of those instructions, to judge whether calls repeat frame to
//! frame. Output is local measurement data and never belongs in the repository.
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Records logged per probed PC; a loop head must not flood the file.
const CAP: u32 = 400_000;

pub(super) fn pcs() -> &'static [u32] {
    static PCS: OnceLock<Vec<u32>> = OnceLock::new();
    PCS.get_or_init(|| {
        if std::env::var_os("ADV_MEMO_PROBE_OUT").is_none() {
            return Vec::new();
        }
        std::env::var("ADV_MEMO_PROBE")
            .unwrap_or_default()
            .split(',')
            .filter_map(|pc| u32::from_str_radix(pc.trim().trim_start_matches("0x"), 16).ok())
            .collect()
    })
}

struct Log {
    out: std::io::BufWriter<std::fs::File>,
    start: Instant,
    counts: HashMap<u32, u32>,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// # Safety
/// `mem` must address the `len`-byte guest window that starts at `base`.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe extern "C" fn record(
    mem: *const u8,
    base: u32,
    len: u32,
    pc: u32,
    eax: u32,
    ecx: u32,
    edx: u32,
    ebx: u32,
    esp: u32,
    ebp: u32,
    esi: u32,
    edi: u32,
) {
    let regs = [eax, ecx, edx, ebx, esp, ebp, esi, edi];
    let bytes = |addr: u32, size: u32| -> Option<&[u8]> {
        let offset = addr.checked_sub(base)?;
        (u64::from(offset) + u64::from(size) <= u64::from(len))
            .then(|| unsafe { std::slice::from_raw_parts(mem.add(offset as usize), size as usize) })
    };
    let word = |addr| bytes(addr, 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
    let mut hasher = DefaultHasher::new();
    regs.hash(&mut hasher);
    let registers = hasher.finish();
    let mut hasher = DefaultHasher::new();
    bytes(esp, 0x40).hash(&mut hasher);
    let stack = hasher.finish();
    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if log.is_none() {
        let Some(path) = std::env::var_os("ADV_MEMO_PROBE_OUT") else {
            return;
        };
        let Ok(file) = std::fs::File::create(path) else {
            return;
        };
        *log = Some(Log {
            out: std::io::BufWriter::with_capacity(1 << 20, file),
            start: Instant::now(),
            counts: HashMap::new(),
        });
    }
    let log = log.as_mut().unwrap();
    let count = log.counts.entry(pc).or_default();
    if *count >= CAP {
        return;
    }
    *count += 1;
    let nanos = log.start.elapsed().as_nanos();
    let _ = writeln!(
        log.out,
        "{nanos}\t{pc:x}\t{:x}\t{esp:x}\t{registers:016x}\t{stack:016x}\t{}",
        word(esp),
        regs.map(|r| format!("{r:x}")).join("\t"),
    );
    if *count % 512 == 0 {
        let _ = log.out.flush();
    }
}
