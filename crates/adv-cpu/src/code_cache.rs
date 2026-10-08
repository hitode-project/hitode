//! Bounded local native artifacts. Corrupt or incompatible input is a cache miss.
//! The execution owner loads and atomically saves the archive; compile workers
//! only exchange immutable records in memory, never perform foreground disk I/O.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::compile::{self, Compiled, Snapshot};
use crate::{CompileStrategy, Region};

const MAGIC: &[u8; 8] = b"ADVCODE1";
const LIMIT: usize = 128 * 1024 * 1024;
const ENTRIES: usize = 4096;
const RECORD_LIMIT: usize = 4 * 1024 * 1024;

struct Record {
    key: Vec<u8>,
    region: Region,
    native: Arc<[u8]>,
    witnessed: bool,
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
        let cache = Arc::new(Self {
            path: path.to_owned(),
            records: Mutex::default(),
            counters: Counters::default(),
        });
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
            if let Some(record) = self.records.lock().unwrap().entries.get_mut(&digest(&key)) {
                if record.key == key {
                    record.witnessed = true;
                }
            }
        }
    }

    pub(crate) fn restore(&self, input: &Snapshot) -> Option<Compiled> {
        let key = match input.cache_key() {
            Ok(key) => key,
            Err(_) => {
                self.counters.unsupported.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        let native = {
            let records = self.records.lock().unwrap();
            records
                .entries
                .get(&digest(&key))
                .filter(|record| record.key == key)
                .map(|record| record.native.clone())
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
        let id = digest(&key);
        let mut witnessed = false;
        if let Some(old) = records.entries.remove(&id) {
            records.bytes -= old.size();
            witnessed = old.witnessed;
        }
        // Preserve already seen records rather than making shutdown perform an
        // unbounded eviction/write pass. A later format version may add an LRU.
        if records.entries.len() >= ENTRIES || records.bytes + size > LIMIT - 1024 * 1024 {
            return;
        }
        records.bytes += size;
        records.entries.insert(
            id,
            Record {
                key,
                region: (*input.region).clone(),
                native: bytes.into(),
                witnessed,
            },
        );
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
                "\"corrupt\":{},\"stored\":{},\"unsupported\":{},\"restore_ns\":{},\"entries\":{},\"bytes\":{}}}"
            ),
            read(&self.counters.hits),
            read(&self.counters.misses),
            read(&self.counters.rejected),
            read(&self.counters.corrupt),
            read(&self.counters.stored),
            read(&self.counters.unsupported),
            read(&self.counters.restored_ns),
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
    for record in records.entries.values() {
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
        let record = Record {
            key,
            region,
            native,
            witnessed: flags & 8 != 0,
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
