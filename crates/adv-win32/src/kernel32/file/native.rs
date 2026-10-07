//! Bounded live NT file callbacks over the existing guest address space. Opaque
//! handles stay with their provider; callers choose their own missing-file policy.
use crate::Win32;
use adv_abi::{guest::check_range, warn_unavailable};
use adv_cpu::backend::{GuestExt, MemFault, Prot};
use adv_machine::{Env, Outcome};
use adv_os::{Heaps, LoadError, Loader, Modules};
use adv_pe::ImportName;

fn trace(env: &mut Env<'_>, name: &str, args: &[u32], status: u32) {
    env.sys.trace.record(&adv_machine::trace::TraceRecord {
        ctx: env.context,
        frame: env.sys.frame,
        module: "<file>",
        name,
        args,
        ret: u64::from(status),
    });
}
const CHUNK: u32 = 64 * 1024;
const HANDLE: u32 = 0;
const IOSB: u32 = 8;
const OFFSET: u32 = 16;
const BUFFER: u32 = 24;
const NO_MEMORY: u32 = 0xc0000017;
const ACCESS_VIOLATION: u32 = 0xc0000005;
const INVALID_PARAMETER: u32 = 0xc000000d;
const NOT_SUPPORTED: u32 = 0xc00000bb;
const END_OF_FILE: u32 = 0xc0000011;
pub(in crate::kernel32) const FILE_TOO_LARGE: u32 = 0xc0000904;

pub(in crate::kernel32) fn win32_error(status: u32) -> u32 {
    match status {
        0 => 0,
        ACCESS_VIOLATION => 998,
        0xc0000008 => 6,
        INVALID_PARAMETER => 87,
        NO_MEMORY | 0xc000009a => 8,
        0xc0000022 => 5,
        0xc0000043 => 32,
        NOT_SUPPORTED => 50,
        0xc000007b => 193,
        0xc000000f | 0xc0000034 => 2,
        0xc000003a => 3,
        0xc0000033 => 123,
        END_OF_FILE => 38,
        FILE_TOO_LARGE => 223,
        _ => 31,
    }
}

/// One process-heap allocation: OBJECT_ATTRIBUTES, counted Unicode name and
/// an eight-byte-aligned caller output area. No second handle or file owner.
pub(in crate::kernel32) struct Attributes {
    pub allocation: u32,
    pub object: u32,
    pub extra: u32,
}
impl Attributes {
    pub(in crate::kernel32) fn free(self, env: &mut Env<'_>) {
        env.ext
            .get_mut::<Heaps>()
            .free(env.cpu, &mut env.sys.vm, 0, self.allocation);
    }
}
pub(in crate::kernel32) fn attributes(
    env: &mut Env<'_>,
    path: &str,
    extra_bytes: u32,
) -> Result<Attributes, u32> {
    let native = if path.starts_with(r"\??\") {
        path.to_owned()
    } else if let Some(extended) = path.strip_prefix(r"\\?\") {
        format!(r"\??\{extended}")
    } else if let Some(unc) = path.strip_prefix(r"\\") {
        format!(r"\??\UNC\{unc}")
    } else {
        format!(r"\??\{path}")
    };
    let units: Vec<_> = native.encode_utf16().collect();
    let length = units.len() * 2;
    if length > u16::MAX as usize - 2 || units.contains(&0) {
        return Err(0xc0000033);
    }
    let extra = (32 + length as u32 + 2 + 7) & !7;
    let size = extra.checked_add(extra_bytes).ok_or(NO_MEMORY)?;
    let allocation = env
        .ext
        .get_mut::<Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, 0, size, true)
        .ok_or(NO_MEMORY)?;
    let result = Attributes {
        allocation,
        object: allocation,
        extra: allocation + extra,
    };
    let initialized = (|| -> Result<(), MemFault> {
        check_range(env.cpu, allocation, size, Prot::WRITE)?;
        env.cpu.write_u32(allocation, 24)?;
        env.cpu.write_u32(allocation + 8, allocation + 24)?;
        env.cpu.write_u32(allocation + 12, 0x40)?;
        env.cpu
            .write(allocation + 24, &(length as u16).to_le_bytes())?;
        env.cpu
            .write(allocation + 26, &(length as u16 + 2).to_le_bytes())?;
        env.cpu.write_u32(allocation + 28, allocation + 32)?;
        let bytes: Vec<_> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        env.cpu.write(allocation + 32, &bytes)
    })();
    if initialized.is_err() {
        result.free(env);
        return Err(ACCESS_VIOLATION);
    }
    Ok(result)
}

pub(in crate::kernel32) fn overridden(
    env: &mut Env<'_>,
    name: &str,
) -> Result<Option<u32>, LoadError> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    let Some(module) = modules.by_name("ntdll.dll") else {
        return Ok(None);
    };
    Loader::new(env.cpu, env.sys, modules, &win32.vfs).guest_override(module, name)
}

pub(in crate::kernel32) fn active(env: &mut Env<'_>) -> Result<bool, LoadError> {
    for name in ["NtCreateFile", "NtOpenFile", "NtReadFile"] {
        if overridden(env, name)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(in crate::kernel32) fn entry(env: &mut Env<'_>, name: &str) -> Result<u32, u32> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    let module = modules.by_name("ntdll.dll").ok_or(NOT_SUPPORTED)?;
    let mut loader = Loader::new(env.cpu, env.sys, modules, &win32.vfs);
    // Reject changed forwarders before resolution can queue initializers.
    if let Some(target) = loader
        .guest_override(module, name)
        .map_err(|_| NOT_SUPPORTED)?
    {
        return Ok(target);
    }
    loader
        .resolve_dynamic(
            module,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .map(|(target, _)| target)
        .map_err(|_| NOT_SUPPORTED)
}

pub(in crate::kernel32) type Completion =
    Box<dyn FnOnce(&mut Env<'_>, Result<Vec<u8>, u32>) -> Outcome>;
struct Read {
    attributes: Attributes,
    limit: usize,
    buffer: u32,
    handle: u32,
    bytes: Vec<u8>,
    then: Completion,
}
impl Read {
    fn finish(self, env: &mut Env<'_>, result: Result<Vec<u8>, u32>) -> Outcome {
        self.attributes.free(env);
        (self.then)(env, result)
    }
    fn close(self, env: &mut Env<'_>, result: Result<Vec<u8>, u32>) -> Outcome {
        let target = match entry(env, "NtClose") {
            Ok(target) => target,
            Err(error) => {
                warn_unavailable(
                    env,
                    "kernel32.dll!<native file reader>",
                    "guest native file cannot be closed: live NtClose is unavailable",
                );
                return self.finish(env, Err(error));
            }
        };
        Outcome::call(target, [self.handle], move |env, resumed| {
            trace(env, "native_close", &[self.handle], resumed.value());
            if resumed.value() == 0 {
                self.finish(env, result)
            } else {
                warn_unavailable(
                    env,
                    "kernel32.dll!<native file reader>",
                    "guest native file close failed",
                );
                self.finish(env, Err(resumed.value()))
            }
        })
    }
    fn next(mut self, env: &mut Env<'_>) -> Outcome {
        let target = match entry(env, "NtReadFile") {
            Ok(target) => target,
            Err(error) => return self.close(env, Err(error)),
        };
        let capacity = self
            .limit
            .saturating_sub(self.bytes.len())
            .saturating_add(1)
            .min(CHUNK as usize) as u32;
        if check_range(env.cpu, self.attributes.extra + IOSB, 8, Prot::WRITE).is_err()
            || check_range(env.cpu, self.attributes.extra + OFFSET, 8, Prot::WRITE).is_err()
            || check_range(env.cpu, self.buffer, capacity, Prot::WRITE).is_err()
            || env
                .cpu
                .write(self.attributes.extra + IOSB, &[0; 8])
                .is_err()
            || env
                .cpu
                .write(
                    self.attributes.extra + OFFSET,
                    &(self.bytes.len() as u64).to_le_bytes(),
                )
                .is_err()
        {
            return self.close(env, Err(ACCESS_VIOLATION));
        }
        let args = [
            self.handle,
            0,
            0,
            0,
            self.attributes.extra + IOSB,
            self.buffer,
            capacity,
            self.attributes.extra + OFFSET,
            0,
        ];
        Outcome::call(target, args, move |env, resumed| {
            let status = resumed.value();
            let reported = env
                .cpu
                .read_u32(self.attributes.extra + IOSB + 4)
                .unwrap_or(u32::MAX);
            trace(
                env,
                "native_read",
                &[self.handle, self.bytes.len() as u32, capacity, reported],
                status,
            );
            if status == END_OF_FILE {
                let bytes = std::mem::take(&mut self.bytes);
                return self.close(env, Ok(bytes));
            }
            if status != 0 {
                if status == 0x103 {
                    // The provider may still own the writable IOSB and buffer.
                    // Stop without reusing them or pretending to cancel I/O.
                    return Outcome::Fail("native file: pending guest file I/O requires native asynchronous completion support".into());
                }
                return self.close(env, Err(status));
            }
            if check_range(env.cpu, self.attributes.extra + IOSB, 8, Prot::READ).is_err() {
                return self.close(env, Err(ACCESS_VIOLATION));
            }
            let Ok(count) = env.cpu.read_u32(self.attributes.extra + IOSB + 4) else {
                return self.close(env, Err(ACCESS_VIOLATION));
            };
            if count > capacity {
                warn_unavailable(
                    env,
                    "kernel32.dll!<native file reader>",
                    "native file callback reported more bytes than the supplied buffer",
                );
                return self.close(env, Err(INVALID_PARAMETER));
            }
            if count == 0 {
                let bytes = std::mem::take(&mut self.bytes);
                return self.close(env, Ok(bytes));
            }
            if check_range(env.cpu, self.buffer, count, Prot::READ).is_err() {
                return self.close(env, Err(ACCESS_VIOLATION));
            }
            if count as usize > self.limit.saturating_sub(self.bytes.len()) {
                return self.close(env, Err(FILE_TOO_LARGE));
            }
            // Keep the whole-file result bounded by both the caller's budget
            // and x86 storage. The provider never controls an unchecked host
            // allocation or pointer arithmetic.
            if self.bytes.len() as u64 + u64::from(count) > u64::from(u32::MAX)
                || self.bytes.try_reserve(count as usize).is_err()
            {
                return self.close(env, Err(NO_MEMORY));
            }
            let old = self.bytes.len();
            self.bytes.resize(old + count as usize, 0);
            if env.cpu.read(self.buffer, &mut self.bytes[old..]).is_err() {
                return self.close(env, Err(ACCESS_VIOLATION));
            }
            self.next(env)
        })
    }
}

pub(in crate::kernel32) fn read(
    env: &mut Env<'_>,
    path: String,
    limit: usize,
    then: Completion,
) -> Outcome {
    // NtOpenFile and NtCreateFile are separate observable entry points. Prefer
    // the hooked create entry, or a hooked open entry if that is the provider.
    let use_open = match (
        overridden(env, "NtCreateFile"),
        overridden(env, "NtOpenFile"),
    ) {
        (Ok(create), Ok(open)) => create.is_none() && open.is_some(),
        _ => return then(env, Err(ACCESS_VIOLATION)),
    };
    let target = match entry(
        env,
        if use_open {
            "NtOpenFile"
        } else {
            "NtCreateFile"
        },
    ) {
        Ok(target) => target,
        Err(error) => return then(env, Err(error)),
    };
    let attributes = match attributes(env, &path, BUFFER + CHUNK) {
        Ok(attributes) => attributes,
        Err(error) => return then(env, Err(error)),
    };
    let scratch = attributes.extra;
    let object = attributes.object;
    let read = Read {
        buffer: scratch + BUFFER,
        attributes,
        limit,
        handle: 0,
        bytes: Vec::new(),
        then,
    };
    let args = if use_open {
        vec![
            scratch + HANDLE,
            0x80100000,
            object,
            scratch + IOSB,
            5,
            0x60,
        ]
    } else {
        vec![
            scratch + HANDLE,
            0x80100000,
            object,
            scratch + IOSB,
            0,
            0,
            5,
            1,
            0x60,
            0,
            0,
        ]
    };
    Outcome::call(target, args, move |env, resumed| {
        let mut read = read;
        trace(
            env,
            "native_open",
            &[env.cpu.read_u32(scratch + HANDLE).unwrap_or(0)],
            resumed.value(),
        );
        match resumed.value() {
            0 if check_range(env.cpu, scratch + HANDLE, 4, Prot::READ).is_err() => {
                read.finish(env, Err(ACCESS_VIOLATION))
            }
            0 => match env.cpu.read_u32(scratch + HANDLE) {
                Ok(handle) if !matches!(handle, 0 | u32::MAX) => {
                    read.handle = handle;
                    read.next(env)
                }
                _ => read.finish(env, Err(INVALID_PARAMETER)),
            },
            0x103 => Outcome::Fail(
                "native file: pending guest file open requires native asynchronous completion support"
                    .into(),
            ),
            status => read.finish(env, Err(status)),
        }
    })
}

#[cfg(test)]
mod tests;
