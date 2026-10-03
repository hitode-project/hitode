//! Multimedia file I/O for guest-owned memory and read-only VFS files.
//! MMIO positions and RIFF sizes are byte offsets, never host pointers. Memory
//! files retain the caller's storage and observe subsequent guest writes.
use std::io::{Read, Seek, SeekFrom};

use adv_abi::Registry;
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::{Conv, Env, Outcome};
use adv_win32::{
    Win32,
    util::{cstr, wstr},
};

const MEM: u32 = u32::from_le_bytes(*b"MEM ");
const DOS: u32 = u32::from_le_bytes(*b"DOS ");
const RIFF: u32 = u32::from_le_bytes(*b"RIFF");
const LIST: u32 = u32::from_le_bytes(*b"LIST");
const INVALID_HANDLE: u32 = 5;
const INVALID_PARAM: u32 = 11;
const CANNOT_READ: u32 = 261;
const CANNOT_WRITE: u32 = 262;
const CANNOT_SEEK: u32 = 263;
const CHUNK_NOT_FOUND: u32 = 265;

enum Source {
    Memory {
        address: u32,
        capacity: u32,
        owned: bool,
    },
    File(std::fs::File),
}
struct File {
    source: Source,
    len: u32,
    position: u32,
    flags: u32,
    info: [u32; 18],
}
/// An HMMIO in the handle table. The file is taken out while a call works on it, so the
/// call can borrow guest memory and the rest of the environment freely.
struct Hmmio(Option<File>);
impl File {
    fn seek(&mut self, offset: i64, origin: u32) -> Option<u32> {
        let offset = if origin == 2 && matches!(self.source, Source::Memory { .. }) {
            // Native MEM I/O interprets END as len - offset; DOS uses len + offset.
            -offset
        } else {
            offset
        };
        let base = match origin {
            0 => 0,
            1 => self.position,
            2 => self.len,
            _ => return None,
        };
        let limit = if let Source::Memory { capacity, .. } = self.source {
            capacity
        } else {
            i32::MAX as u32
        };
        let at = i64::from(base).checked_add(offset)?;
        if !(0..=i64::from(limit)).contains(&at) {
            return None;
        }
        self.position = at as u32;
        Some(self.position)
    }
    fn copy_out(&mut self, cpu: &mut dyn Guest, target: u32, count: u32) -> Result<u32, ()> {
        let count = count.min(self.len.saturating_sub(self.position));
        target.checked_add(count).ok_or(())?;
        if let Source::Memory { address, .. } = self.source {
            copy_guest(
                cpu,
                address.checked_add(self.position).ok_or(())?,
                target,
                count,
            )?;
            self.position += count;
            return Ok(count);
        }
        let mut buffer = [0u8; 16_384];
        let mut copied = 0;
        while copied < count {
            let n = (count - copied).min(buffer.len() as u32) as usize;
            let got = self
                .read_at(cpu, self.position, &mut buffer[..n])
                .map_err(|_| ())?;
            if got == 0 {
                break;
            }
            cpu.write(target + copied, &buffer[..got]).map_err(|_| ())?;
            self.position += got as u32;
            copied += got as u32;
        }
        Ok(copied)
    }
    fn read_at(&mut self, cpu: &dyn Guest, at: u32, out: &mut [u8]) -> Result<usize, u32> {
        let count = out.len().min(self.len.saturating_sub(at) as usize);
        match &mut self.source {
            Source::Memory { address, .. } => {
                cpu.read(
                    address.checked_add(at).ok_or(CANNOT_READ)?,
                    &mut out[..count],
                )
                .map_err(|_| CANNOT_READ)?;
                Ok(count)
            }
            Source::File(file) => {
                file.seek(SeekFrom::Start(u64::from(at)))
                    .map_err(|_| CANNOT_READ)?;
                file.read(&mut out[..count]).map_err(|_| CANNOT_READ)
            }
        }
    }
    fn words(&self, handle: u32) -> [u32; 18] {
        let mut info = self.info;
        info[0] = self.flags;
        if matches!(self.source, Source::File(_)) {
            info[11] = self.position;
        }
        info[17] = handle;
        if let Source::Memory {
            address, capacity, ..
        } = self.source
        {
            info[5] = capacity;
            info[6] = address;
            info[7] = address.saturating_add(self.position);
            info[8] = address.saturating_add(self.len);
            info[9] = address.saturating_add(capacity);
        }
        info
    }
}
fn copy_guest(cpu: &mut dyn Guest, source: u32, target: u32, count: u32) -> Result<(), ()> {
    source.checked_add(count).ok_or(())?;
    target.checked_add(count).ok_or(())?;
    let backwards = target > source && u64::from(target) < u64::from(source) + u64::from(count);
    let mut buffer = [0u8; 16_384];
    let mut copied = 0;
    while copied < count {
        let n = (count - copied).min(buffer.len() as u32);
        let offset = if backwards {
            count - copied - n
        } else {
            copied
        };
        cpu.read(source + offset, &mut buffer[..n as usize])
            .map_err(|_| ())?;
        cpu.write(target + offset, &buffer[..n as usize])
            .map_err(|_| ())?;
        copied += n;
    }
    Ok(())
}
fn words(cpu: &dyn Guest, p: u32, count: usize) -> Option<Vec<u32>> {
    let mut raw = vec![0; count * 4];
    cpu.read(p, &mut raw).ok()?;
    Some(
        raw.as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect(),
    )
}
fn put_words(cpu: &mut dyn Guest, p: u32, values: &[u32]) -> bool {
    let raw: Vec<_> = values.iter().flat_map(|w| w.to_le_bytes()).collect();
    cpu.write(p, &raw).is_ok()
}
fn open_error(e: &mut Env<'_>, info: u32, error: u32) -> Outcome {
    if info != 0 {
        let _ = e.cpu.write_u32(info + 12, error);
    }
    Outcome::Return(0)
}
fn open(e: &mut Env<'_>) -> Outcome {
    let (name, info, flags) = (e.arg(0), e.arg(1), e.arg(2));
    let mut fields = [0u32; 18];
    if info != 0 {
        let Some(values) = words(e.cpu, info, 18) else {
            return Outcome::Return(0);
        };
        fields.copy_from_slice(&values);
    }
    // Custom guest I/O callbacks and file creation require a separate persona;
    // never accept them while silently using a different datasource.
    if fields[2] != 0 || flags & (0x100 | 0x200 | 0x4000 | 0x20000) != 0 {
        return open_error(e, info, 272);
    }
    let (source, len) = if fields[1] == MEM {
        let capacity = fields[5];
        if capacity > i32::MAX as u32 || fields[12] != 0 {
            return open_error(e, info, 272);
        }
        let owned = fields[6] == 0;
        let address = if owned {
            let Some(address) =
                e.ext
                    .get_mut::<Win32>()
                    .heaps
                    .alloc(e.cpu, &mut e.sys.vm, 0, capacity, true)
            else {
                return open_error(e, info, 258);
            };
            address
        } else {
            fields[6]
        };
        if address.checked_add(capacity).is_none() {
            if owned {
                e.ext
                    .get_mut::<Win32>()
                    .heaps
                    .free(e.cpu, &mut e.sys.vm, 0, address);
            }
            return open_error(e, info, 272);
        }
        (
            Source::Memory {
                address,
                capacity,
                owned,
            },
            if flags & 0x1000 != 0 { 0 } else { capacity },
        )
    } else {
        // Read/seek disk files directly. Do not advertise a guest-visible
        // buffered I/O window when no mmioAdvance persona is installed.
        if name == 0 || !matches!(fields[1], 0 | DOS) || flags & (3 | 0x1000 | 0x10000) != 0 {
            return open_error(e, info, 272);
        }
        let wide = e.sys.thunks.get(e.thunk).name.ends_with('W');
        let path = if wide {
            String::from_utf16_lossy(&wstr(e.cpu, name))
        } else {
            e.ext.get::<Win32>().codec.decode(&cstr(e.cpu, name))
        };
        let Some(path) = e.ext.get::<Win32>().vfs.lookup(&path) else {
            return open_error(e, info, 257);
        };
        let Ok(file) = std::fs::File::open(path) else {
            return open_error(e, info, 259);
        };
        let Ok(len) = file
            .metadata()
            .and_then(|m| u32::try_from(m.len()).map_err(std::io::Error::other))
        else {
            return open_error(e, info, 272);
        };
        if len > i32::MAX as u32 {
            return open_error(e, info, 272);
        }
        (Source::File(file), len)
    };
    let handle = e.sys.objects.insert_host(Hmmio(None));
    let mut file = File {
        source,
        len,
        position: 0,
        flags,
        info: fields,
    };
    if matches!(file.source, Source::Memory { owned: true, .. }) {
        file.flags |= 0x10000; // Internally allocated buffer.
    }
    if let Some(path) = std::env::var_os("ADV_MMIO_TRACE") {
        use std::io::Write;
        let mut magic = [0u8; 64];
        let got = file.read_at(e.cpu, 0, &mut magic).unwrap_or(0);
        if let Ok(mut trace) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(
                trace,
                "open us={} handle={handle:x} bytes={len} flags={flags:x} prefix={:02x?}",
                e.sys.clock.now_us(),
                &magic[..got]
            );
        }
    }
    put_back(e, handle, file);
    if info != 0 {
        let _ = e.cpu.write_u32(info + 12, 0);
    }
    Outcome::Return(handle)
}
fn call(e: &mut Env<'_>) -> Outcome {
    let name = e.sys.thunks.get(e.thunk).name.clone();
    if name.starts_with("mmioOpen") {
        return open(e);
    }
    if name.starts_with("mmioStringToFOURCC") {
        let mut four = [b' '; 4];
        if name.ends_with('W') {
            for (d, s) in four.iter_mut().zip(wstr(e.cpu, e.arg(0))) {
                *d = s as u8;
            }
        } else {
            for (d, s) in four.iter_mut().zip(cstr(e.cpu, e.arg(0))) {
                *d = s;
            }
        }
        if e.arg(1) & 0x10 != 0 {
            four.make_ascii_uppercase();
        }
        return Outcome::Return(u32::from_le_bytes(four));
    }
    let handle = e.arg(0);
    if name == "mmioClose" {
        let flags = e.arg(1);
        if flags & !0x10 != 0 {
            return Outcome::Return(10);
        }
        let Some(file) = take(e, handle) else {
            return Outcome::Return(INVALID_HANDLE);
        };
        e.sys.objects.close(handle);
        if let Source::Memory {
            address,
            owned: true,
            ..
        } = file.source
        {
            e.ext
                .get_mut::<Win32>()
                .heaps
                .free(e.cpu, &mut e.sys.vm, 0, address);
        }
        return Outcome::Return(0);
    }
    let Some(mut file) = take(e, handle) else {
        return Outcome::Return(
            if matches!(name.as_str(), "mmioSeek" | "mmioRead" | "mmioWrite") {
                u32::MAX
            } else {
                INVALID_HANDLE
            },
        );
    };
    let value = match name.as_str() {
        "mmioSeek" => file
            .seek(i64::from(e.arg(1) as i32), e.arg(2))
            .unwrap_or(u32::MAX),
        "mmioRead" => {
            let count = e.arg(2) as i32;
            if count < 0 {
                u32::MAX
            } else {
                let target = e.arg(1);
                file.copy_out(e.cpu, target, count as u32)
                    .unwrap_or(u32::MAX)
            }
        }
        "mmioWrite" => {
            let count = e.arg(2) as i32;
            if count < 0 || file.flags & 3 == 0 {
                u32::MAX
            } else if let Source::Memory {
                address, capacity, ..
            } = file.source
            {
                let n = (count as u32).min(capacity.saturating_sub(file.position));
                let source = e.arg(1);
                if copy_guest(e.cpu, source, address + file.position, n).is_ok() {
                    file.position += n;
                    file.len = file.len.max(file.position);
                    file.flags |= 0x1000_0000;
                    n
                } else {
                    u32::MAX
                }
            } else {
                u32::MAX
            }
        }
        "mmioGetInfo" => {
            let out = e.arg(1);
            if e.arg(2) != 0 || !put_words(e.cpu, out, &file.words(handle)) {
                INVALID_PARAM
            } else {
                0
            }
        }
        "mmioSetInfo" => match words(e.cpu, e.arg(1), 18) {
            Some(info) if e.arg(2) == 0 => {
                if let Source::Memory {
                    address, capacity, ..
                } = file.source
                {
                    match info[7].checked_sub(address).filter(|&at| at <= capacity) {
                        Some(at) => {
                            file.position = at;
                            0
                        }
                        _ => INVALID_PARAM,
                    }
                } else {
                    INVALID_PARAM
                }
            }
            _ => INVALID_PARAM,
        },
        "mmioDescend" => descend(e, &mut file),
        "mmioAscend" => {
            if e.arg(2) != 0 {
                INVALID_PARAM
            } else if let Some(ck) = words(e.cpu, e.arg(1), 5) {
                if ck[4] & 0x1000_0000 != 0 {
                    CANNOT_WRITE
                } else {
                    match u64::from(ck[3]).checked_add(u64::from(ck[1]) + u64::from(ck[1] & 1)) {
                        Some(at) if at <= i32::MAX as u64 => {
                            file.seek(at as i64, 0).map_or(CANNOT_SEEK, |_| 0)
                        }
                        _ => CANNOT_SEEK,
                    }
                }
            } else {
                INVALID_PARAM
            }
        }
        _ => INVALID_PARAM,
    };
    put_back(e, handle, file);
    Outcome::Return(value)
}
fn take(e: &mut Env<'_>, handle: u32) -> Option<File> {
    e.sys.objects.host_mut::<Hmmio>(handle)?.0.take()
}
fn put_back(e: &mut Env<'_>, handle: u32, file: File) {
    e.sys.objects.host_mut::<Hmmio>(handle).unwrap().0 = Some(file);
}
fn descend(e: &mut Env<'_>, file: &mut File) -> u32 {
    let Some(wanted) = words(e.cpu, e.arg(1), 5) else {
        return INVALID_PARAM;
    };
    let flags = e.arg(3);
    let (id, ty) = match flags {
        0 => (None, None),
        0x10 => (Some(wanted[0]), None),
        0x20 => (Some(RIFF), Some(wanted[2])),
        0x40 => (Some(LIST), Some(wanted[2])),
        _ => return INVALID_PARAM,
    };
    let parent = if e.arg(2) != 0 {
        let Some(parent) = words(e.cpu, e.arg(2), 5) else {
            return INVALID_PARAM;
        };
        Some((
            u64::from(parent[3]),
            u64::from(parent[3]) + u64::from(parent[1]),
        ))
    } else {
        None
    };
    let limit = parent.map_or(u64::from(file.len), |(_, end)| end.min(u64::from(file.len)));
    if parent.is_some_and(|(start, end)| {
        u64::from(file.position) < start || u64::from(file.position) >= end
    }) {
        return CHUNK_NOT_FOUND;
    }
    loop {
        let at = u64::from(file.position);
        if at + 8 > limit {
            return CHUNK_NOT_FOUND;
        }
        let mut header = [0u8; 12];
        if file.read_at(e.cpu, file.position, &mut header[..8]).ok() != Some(8) {
            return CHUNK_NOT_FOUND;
        }
        let ckid = u32::from_le_bytes(header[..4].try_into().unwrap());
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap());
        file.position += 8;
        let data = file.position;
        let fcc = if matches!(ckid, RIFF | LIST) {
            if size < 4
                || u64::from(data) + 4 > limit
                || file.read_at(e.cpu, data, &mut header[8..12]).ok() != Some(4)
            {
                return CHUNK_NOT_FOUND;
            }
            file.position += 4;
            u32::from_le_bytes(header[8..12].try_into().unwrap())
        } else {
            0
        };
        if id.is_none_or(|id| ckid == id) && ty.is_none_or(|ty| ty == fcc) {
            let out = e.arg(1);
            return if put_words(e.cpu, out, &[ckid, size, fcc, data, 0]) {
                0
            } else {
                INVALID_PARAM
            };
        }
        let next = u64::from(data) + u64::from(size) + u64::from(size & 1);
        if next > i32::MAX as u64 {
            return CHUNK_NOT_FOUND;
        }
        file.position = next as u32;
    }
}
pub fn register(r: &mut Registry) {
    for (name, argc) in [
        ("mmioOpenA", 3),
        ("mmioOpenW", 3),
        ("mmioClose", 2),
        ("mmioRead", 3),
        ("mmioWrite", 3),
        ("mmioSeek", 3),
        ("mmioDescend", 4),
        ("mmioAscend", 3),
        ("mmioGetInfo", 3),
        ("mmioSetInfo", 3),
        ("mmioStringToFOURCCA", 2),
        ("mmioStringToFOURCCW", 2),
    ] {
        r.add("winmm.dll", name, Conv::Stdcall { argc }, call);
    }
}
