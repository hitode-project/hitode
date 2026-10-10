//! Opt-in anonymous host-code ranges for external CPU samplers.
//!
//! ADV_PERF_MAP_DIR writes perf-{pid}.map and perf-{pid}.events.tsv. The map
//! uses perf's hexadecimal address/size format; the event sidecar adds monotonic
//! nanoseconds and retirement so offline readers can handle address reuse.
//! Only host addresses and fixed categories are written, never guest metadata.
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy)]
pub enum Category {
    Tier2,
}

impl Category {
    fn name(self) -> &'static str {
        match self {
            Self::Tier2 => "adv-jit-tier2",
        }
    }
}

struct Writer<M, E> {
    map: M,
    events: E,
}

impl<M: Write, E: Write> Writer<M, E> {
    fn event(&mut self, ns: u64, action: &str, range: &Registration) -> io::Result<()> {
        if action == "load" {
            writeln!(
                self.map,
                "{:x} {:x} {}",
                range.base,
                range.size,
                range.category.name()
            )?;
        }
        writeln!(
            self.events,
            "{ns}\t{action}\t{:x}\t{:x}\t{}",
            range.base,
            range.size,
            range.category.name()
        )
    }
}

type Sink = Mutex<Option<Writer<File, File>>>;

fn sink() -> Option<&'static Sink> {
    static SINK: OnceLock<Option<Sink>> = OnceLock::new();
    SINK.get_or_init(|| {
        let directory = std::path::PathBuf::from(std::env::var_os("ADV_PERF_MAP_DIR")?);
        let open = || -> io::Result<_> {
            std::fs::create_dir_all(&directory)?;
            let pid = std::process::id();
            Ok(Writer {
                map: File::create(directory.join(format!("perf-{pid}.map")))?,
                events: File::create(directory.join(format!("perf-{pid}.events.tsv")))?,
            })
        };
        match open() {
            Ok(writer) => Some(Mutex::new(Some(writer))),
            Err(error) => {
                eprintln!("warning: anonymous perf map disabled: {error}");
                None
            }
        }
    })
    .as_ref()
}

fn monotonic_ns() -> u64 {
    #[cfg(unix)]
    {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // Linux/Android perf sample timestamps use this same clock.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0 {
            return time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64;
        }
    }
    // Non-POSIX hosts can consume the map, but not correlate this fallback with
    // Linux perf timestamps. A failed clock query likewise stays diagnostic-only.
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as u64
}

fn event(sink: &Sink, action: &str, range: &Registration) {
    let mut writer = sink.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(output) = writer.as_mut()
        && let Err(error) = output.event(monotonic_ns(), action, range)
    {
        eprintln!("warning: anonymous perf map disabled after write failure: {error}");
        *writer = None;
    }
}

/// Kept by the code owner and retired before its executable storage is freed.
pub struct Registration {
    base: usize,
    size: usize,
    category: Category,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(sink) = sink() {
            event(sink, "unload", self);
        }
    }
}

pub fn register(base: usize, size: usize, category: Category) -> Option<Registration> {
    let sink = sink()?;
    // Android allocator pointers can carry a top-byte tag; instruction PCs do
    // not. Perf maps must use the canonical host address, not the pointer tag.
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    let base = base & 0x00ff_ffff_ffff_ffff;
    if size == 0 || base.checked_add(size).is_none() {
        return None;
    }
    let range = Registration {
        base,
        size,
        category,
    };
    event(sink, "load", &range);
    Some(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_map_and_lifetime_sidecar() {
        let range = std::mem::ManuallyDrop::new(Registration {
            base: 0x1000,
            size: 0x20,
            category: Category::Tier2,
        });
        let mut writer = Writer {
            map: Vec::new(),
            events: Vec::new(),
        };
        writer.event(10, "load", &range).unwrap();
        writer.event(20, "unload", &range).unwrap();
        assert_eq!(
            String::from_utf8(writer.map).unwrap(),
            "1000 20 adv-jit-tier2\n"
        );
        assert_eq!(
            String::from_utf8(writer.events).unwrap(),
            "10\tload\t1000\t20\tadv-jit-tier2\n20\tunload\t1000\t20\tadv-jit-tier2\n"
        );
    }

    #[test]
    fn failed_map_write_does_not_emit_a_valid_load_event() {
        let range = std::mem::ManuallyDrop::new(Registration {
            base: 0x1000,
            size: 4,
            category: Category::Tier2,
        });
        let mut writer = Writer {
            map: io::Cursor::new([]),
            events: Vec::new(),
        };
        assert!(writer.event(10, "load", &range).is_err());
        assert!(writer.events.is_empty());
    }
}
