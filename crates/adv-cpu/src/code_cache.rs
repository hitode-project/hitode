//! Bounded local native artifacts. Corrupt or incompatible input is a cache miss.
//! The execution owner loads and atomically saves the archive; compile workers
//! only exchange immutable records in memory, never perform foreground disk I/O.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};

use sha2::{Digest, Sha256};

use crate::compile::{self, Compiled, Snapshot};
use crate::{CompileStrategy, Region};

const MAGIC: &[u8; 8] = b"ADVCODE1";
const LIMIT: usize = 128 * 1024 * 1024;
const ENTRIES: usize = 4096;
const RECORD_LIMIT: usize = 4 * 1024 * 1024;

/// Bind native artifacts to the actual host executable, not just a source tag.
/// Failure disables persistence; it never weakens the key or guest execution.
pub(crate) fn host_build_identity() -> Result<[u8; 32], String> {
    static ID: OnceLock<Result<[u8; 32], String>> = OnceLock::new();
    ID.get_or_init(|| {
        let path = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let len = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if len == 0 {
                break;
            }
            hash.update(&buffer[..len]);
        }
        Ok(hash.finalize().into())
    })
    .clone()
}

struct Record {
    key: Vec<u8>,
    region: Region,
    native: Arc<[u8]>,
    witnessed: bool,
    used: u64,
}

impl Record {
    fn size(&self) -> usize {
        self.key.len()
            + self.native.len()
            + 29
            + 4 * (self.region.entries.len()
                + self.region.frontier.len()
                + self.region.callees.len())
    }
}

#[derive(Default)]
struct Records {
    entries: BTreeMap<[u8; 32], Record>,
    bytes: usize,
    clock: u64,
}

impl Records {
    fn insert(&mut self, mut record: Record, byte_limit: usize, entry_limit: usize) -> u64 {
        let size = record.size();
        if size > byte_limit || entry_limit == 0 {
            return 0;
        }
        let id = digest(&record.key);
        if let Some(old) = self.entries.remove(&id) {
            self.bytes -= old.size();
            record.witnessed |= old.witnessed;
        }
        let mut evicted = 0;
        while self.entries.len() >= entry_limit || self.bytes + size > byte_limit {
            let oldest = *self
                .entries
                .iter()
                .min_by_key(|(_, record)| record.used)
                .unwrap()
                .0;
            self.bytes -= self.entries.remove(&oldest).unwrap().size();
            evicted += 1;
        }
        self.clock = self.clock.saturating_add(1);
        record.used = self.clock;
        self.bytes += size;
        self.entries.insert(id, record);
        evicted
    }

    fn get(&mut self, key: &[u8]) -> Option<&mut Record> {
        self.clock = self.clock.saturating_add(1);
        let record = self
            .entries
            .get_mut(&digest(key))
            .filter(|r| r.key == key)?;
        record.used = self.clock;
        Some(record)
    }
}

#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    rejected: AtomicU64,
    corrupt: AtomicU64,
    stored: AtomicU64,
    unsupported: AtomicU64,
    restored_ns: AtomicU64,
    load_ns: AtomicU64,
    evicted: AtomicU64,
}

pub(crate) struct Cache {
    path: PathBuf,
    records: Mutex<Records>,
    counters: Counters,
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

impl Cache {
    pub(crate) fn open(path: &Path) -> Arc<Self> {
        // Overlap executable hashing with guest image loading. A later key
        // lookup still waits for the complete digest before accepting an image.
        static PREPARE: Once = Once::new();
        PREPARE.call_once(|| {
            let _ = std::thread::Builder::new()
                .name("adv-cache-identity".into())
                .spawn(|| {
                    let _ = host_build_identity();
                });
        });
        let cache = Arc::new(Self {
            path: path.to_owned(),
            records: Mutex::default(),
            counters: Counters::default(),
        });
        let started = std::time::Instant::now();
        match std::fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                let valid = file
                    .take((LIMIT + 1) as u64)
                    .read_to_end(&mut bytes)
                    .is_ok()
                    && bytes.len() <= LIMIT;
                if let Some(records) = valid.then(|| parse(&bytes)).flatten() {
                    *cache.records.lock().unwrap() = records;
                } else {
                    cache.counters.corrupt.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                cache.counters.corrupt.fetch_add(1, Ordering::Relaxed);
            }
        }
        cache.counters.load_ns.store(
            started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        cache
    }

    pub(crate) fn regions(&self) -> Vec<Region> {
        self.records
            .lock()
            .unwrap()
            .entries
            .values()
            .filter(|record| record.witnessed)
            .map(|record| record.region.clone())
            .collect()
    }

    pub(crate) fn witness(&self, input: &Snapshot) {
        if let Ok(key) = input.cache_key() {
            if let Some(record) = self.records.lock().unwrap().get(&key) {
                record.witnessed = true;
            }
        }
    }

    pub(crate) fn restore(&self, input: &Snapshot) -> Option<Compiled> {
        if !cfg!(test) && input.cache_guest.is_none() {
            self.counters.unsupported.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if self.records.lock().unwrap().entries.is_empty() {
            self.counters.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let key = match input.cache_key() {
            Ok(key) => key,
            Err(_) => {
                self.counters.unsupported.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        let native = {
            let mut records = self.records.lock().unwrap();
            records.get(&key).map(|record| record.native.clone())
        };
        let Some(native) = native else {
            self.counters.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let started = std::time::Instant::now();
        let compiled = compile::restore_snapshot(input, &native);
        self.counters.restored_ns.fetch_add(
            started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        match compiled {
            Ok(mut compiled) => {
                compiled.discard_cache_image();
                self.counters.hits.fetch_add(1, Ordering::Relaxed);
                Some(compiled)
            }
            Err(_) => {
                self.counters.rejected.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub(crate) fn store(&self, input: &Snapshot, compiled: &Compiled) {
        if !cfg!(test) && input.cache_guest.is_none() {
            self.counters.unsupported.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let (Ok(key), Ok(bytes)) = (input.cache_key(), compiled.cache_bytes()) else {
            self.counters.unsupported.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let size = key
            .len()
            .saturating_add(bytes.len())
            .saturating_add(29)
            .saturating_add(
                4 * (input.region.entries.len()
                    + input.region.frontier.len()
                    + input.region.callees.len()),
            );
        if size > RECORD_LIMIT {
            return;
        }
        let mut records = self.records.lock().unwrap();
        let evicted = records.insert(
            Record {
                key,
                region: (*input.region).clone(),
                native: bytes.into(),
                witnessed: false,
                used: 0,
            },
            LIMIT - 1024 * 1024,
            ENTRIES,
        );
        self.counters.evicted.fetch_add(evicted, Ordering::Relaxed);
        self.counters.stored.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn save(&self) -> std::io::Result<()> {
        let bytes = encode(&self.records.lock().unwrap());
        let parent = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::other("cache has no parent"))?;
        std::fs::create_dir_all(parent)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = self.path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut created = false;
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            created = true;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)
        })();
        if result.is_err() && created {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub(crate) fn json(&self) -> String {
        let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
        let records = self.records.lock().unwrap();
        format!(
            concat!(
                "{{\"enabled\":true,\"hits\":{},\"misses\":{},\"rejected\":{},",
                "\"corrupt\":{},\"stored\":{},\"unsupported\":{},\"restore_ns\":{},\"load_ns\":{},\"evicted\":{},\"entries\":{},\"bytes\":{}}}"
            ),
            read(&self.counters.hits),
            read(&self.counters.misses),
            read(&self.counters.rejected),
            read(&self.counters.corrupt),
            read(&self.counters.stored),
            read(&self.counters.unsupported),
            read(&self.counters.restored_ns),
            read(&self.counters.load_ns),
            read(&self.counters.evicted),
            records.entries.len(),
            records.bytes
        )
    }
}

fn word(out: &mut Vec<u8>, value: usize) {
    out.extend((value as u32).to_le_bytes());
}
fn list(out: &mut Vec<u8>, values: &[u32]) {
    word(out, values.len());
    for value in values {
        out.extend(value.to_le_bytes());
    }
}
fn encode(records: &Records) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    word(&mut out, records.entries.len());
    // File order preserves LRU order without persisting process-local clocks.
    let mut ordered: Vec<_> = records.entries.values().collect();
    ordered.sort_by_key(|record| record.used);
    for record in ordered {
        let region = &record.region;
        out.extend(region.start.to_le_bytes());
        word(&mut out, region.len);
        out.push(
            u8::from(region.reachable)
                | u8::from(region.complete_tail) << 1
                | u8::from(region.compile_strategy == CompileStrategy::Speed) << 2
                | u8::from(record.witnessed) << 3,
        );
        for values in [&region.entries, &region.frontier, &region.callees] {
            list(&mut out, values);
        }
        for bytes in [record.key.as_slice(), record.native.as_ref()] {
            word(&mut out, bytes.len());
            out.extend(bytes);
        }
    }
    out.extend(digest(&out));
    out
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (first, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(first)
    }
    fn word(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn list(&mut self) -> Option<Vec<u32>> {
        let count = self.word()? as usize;
        if count > 16384 || count > self.0.len() / 4 {
            return None;
        }
        (0..count).map(|_| self.word()).collect()
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.word()? as usize;
        if len > RECORD_LIMIT {
            return None;
        }
        self.take(len)
    }
}
fn parse(bytes: &[u8]) -> Option<Records> {
    if bytes.len() > LIMIT || bytes.len() < 44 {
        return None;
    }
    let (body, hash) = bytes.split_at(bytes.len() - 32);
    if digest(body).as_slice() != hash {
        return None;
    }
    let mut reader = Reader(body.strip_prefix(MAGIC)?);
    let count = reader.word()? as usize;
    if count > ENTRIES {
        return None;
    }
    let mut records = Records::default();
    for _ in 0..count {
        let start = reader.word()?;
        let len = reader.word()? as usize;
        if !(1..=16384).contains(&len) || u64::from(start) + len as u64 > 1 << 32 {
            return None;
        }
        let flags = reader.take(1)?[0];
        if flags & !15 != 0 {
            return None;
        }
        let mut region = Region::new(start, len);
        region.reachable = flags & 1 != 0;
        region.complete_tail = flags & 2 != 0;
        region.compile_strategy = if flags & 4 == 0 {
            CompileStrategy::Fast
        } else {
            CompileStrategy::Speed
        };
        region.entries = reader.list()?;
        region.frontier = reader.list()?;
        region.callees = reader.list()?;
        let key = reader.bytes()?.to_vec();
        let native: Arc<[u8]> = reader.bytes()?.into();
        let id = digest(&key);
        records.clock += 1;
        let record = Record {
            key,
            region,
            native,
            witnessed: flags & 8 != 0,
            used: records.clock,
        };
        records.bytes += record.size();
        if records.entries.insert(id, record).is_some() {
            return None;
        }
    }
    reader.0.is_empty().then_some(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_lru_evicts_old_records_and_keeps_recency_across_save() {
        let record = |id| Record {
            key: vec![id],
            region: Region::new(4096, 8),
            native: Arc::from([1, 2, 3]),
            witnessed: false,
            used: 0,
        };
        let mut records = Records::default();
        let limit = record(1).size() * 2;
        records.insert(record(1), limit, 2);
        records.insert(record(2), limit, 2);
        records.get(&[1]).unwrap().witnessed = true;
        let mut records = parse(&encode(&records)).unwrap();
        assert_eq!(records.insert(record(3), limit, 2), 1);
        assert!(records.get(&[2]).is_none());
        assert!(records.get(&[1]).unwrap().witnessed);
        assert_eq!(records.bytes, limit);
        assert_eq!(records.entries.len(), 2);
        records.insert(record(1), limit, 2);
        assert!(records.get(&[1]).unwrap().witnessed);
        assert_eq!(records.insert(record(4), limit, 1), 2);
        assert!(records.bytes <= limit);
        assert_eq!(records.entries.len(), 1);
    }
    #[test]
    fn archive_rejects_corruption_truncation_duplicates_and_excess_counts() {
        let mut records = Records::default();
        let key = vec![1, 2, 3];
        records.entries.insert(
            digest(&key),
            Record {
                key,
                region: Region::new(4096, 8),
                native: Arc::from([4, 5, 6]),
                witnessed: false,
                used: 0,
            },
        );
        let valid = encode(&records);
        assert_eq!(parse(&valid).unwrap().entries.len(), 1);
        for length in 0..valid.len() {
            assert!(parse(&valid[..length]).is_none());
        }
        let mut changed = valid.clone();
        changed[20] ^= 1;
        assert!(parse(&changed).is_none());
        let mut changed = valid.clone();
        changed[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let end = changed.len() - 32;
        let hash = digest(&changed[..end]);
        changed[end..].copy_from_slice(&hash);
        assert!(parse(&changed).is_none());
        let mut changed = valid[..valid.len() - 32].to_vec();
        changed[8..12].copy_from_slice(&2u32.to_le_bytes());
        changed.extend_from_slice(&valid[12..valid.len() - 32]);
        changed.extend(digest(&changed));
        assert!(parse(&changed).is_none());
    }
}
