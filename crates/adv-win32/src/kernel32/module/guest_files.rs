//! Active guest file hooks may own files absent from the backing VFS. Read those
//! through their live NT entries, then reuse the ordinary transactional PE loader.
//! This cache belongs to one load operation, including its dependency graph.
use std::collections::BTreeMap;

use adv_abi::{guest::check_range, warn_unavailable};
use adv_cpu::backend::{GuestExt, MemFault, Prot};
use adv_machine::{Env, Outcome};
use adv_os::{Heaps, LoadError, Loader, Modules, Vfs, loader::FileSource};
use adv_pe::ImportName;

use crate::Win32;

fn trace(env: &mut Env<'_>, name: &str, args: &[u32], status: u32) {
    env.sys.trace.record(&adv_machine::trace::TraceRecord {
        ctx: env.context,
        frame: env.sys.frame,
        module: "<loader>",
        name,
        args,
        ret: u64::from(status),
    });
}

const CHUNK: u32 = 64 * 1024;
const HANDLE: u32 = 0;
const IOSB: u32 = 8;
const ATTRIBUTES: u32 = 16;
const NAME: u32 = 40;
const OFFSET: u32 = 48;
const PATH: u32 = 64;
const NO_MEMORY: u32 = 0xc0000017;
const ACCESS_VIOLATION: u32 = 0xc0000005;
const INVALID_PARAMETER: u32 = 0xc000000d;
const NOT_SUPPORTED: u32 = 0xc00000bb;
const END_OF_FILE: u32 = 0xc0000011;

#[derive(Default)]
pub(super) struct Files(BTreeMap<String, Option<Vec<u8>>>);
impl Files {
    pub(super) fn insert(&mut self, path: String, bytes: Option<Vec<u8>>) {
        self.0.insert(path.to_lowercase(), bytes);
    }
}
pub(super) struct Source<'a> {
    pub files: &'a Files,
    pub vfs: &'a Vfs,
}
impl FileSource for Source<'_> {
    fn absolute_path(&self, path: &str) -> String {
        self.vfs.absolute_path(path)
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files
            .0
            .get(&self.vfs.normalize(path).to_lowercase())
            .cloned()
            .flatten()
    }
    fn read_for_loader(&self, path: &str) -> Result<Option<Vec<u8>>, LoadError> {
        let path = self.vfs.normalize(path);
        self.files
            .0
            .get(&path.to_lowercase())
            .cloned()
            .ok_or(LoadError::DeferredRead(path))
    }
}

fn overridden(env: &mut Env<'_>, name: &str) -> Result<Option<u32>, LoadError> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    let Some(module) = modules.by_name("ntdll.dll") else {
        return Ok(None);
    };
    Loader::new(env.cpu, env.sys, modules, &win32.vfs).guest_override(module, name)
}

pub(super) fn active(env: &mut Env<'_>) -> Result<bool, LoadError> {
    for name in ["NtCreateFile", "NtOpenFile", "NtReadFile"] {
        if overridden(env, name)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn entry(env: &mut Env<'_>, name: &str) -> Result<u32, u32> {
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

type Completion = Box<dyn FnOnce(&mut Env<'_>, Result<Option<Vec<u8>>, u32>) -> Outcome>;
struct Read {
    scratch: u32,
    buffer: u32,
    handle: u32,
    bytes: Vec<u8>,
    then: Completion,
}
impl Read {
    fn finish(self, env: &mut Env<'_>, result: Result<Option<Vec<u8>>, u32>) -> Outcome {
        env.ext
            .get_mut::<Heaps>()
            .free(env.cpu, &mut env.sys.vm, 0, self.scratch);
        (self.then)(env, result)
    }
    fn close(self, env: &mut Env<'_>, result: Result<Option<Vec<u8>>, u32>) -> Outcome {
        let target = match entry(env, "NtClose") {
            Ok(target) => target,
            Err(error) => {
                warn_unavailable(
                    env,
                    "kernel32.dll!LoadLibrary",
                    "guest native file cannot be closed: live NtClose is unavailable",
                );
                return self.finish(env, Err(error));
            }
        };
        Outcome::call(target, [self.handle], move |env, resumed| {
            trace(env, "native_close", &[self.handle], resumed.value());
            if resumed.value() as i32 >= 0 {
                self.finish(env, result)
            } else {
                warn_unavailable(
                    env,
                    "kernel32.dll!LoadLibrary",
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
        if check_range(env.cpu, self.scratch + IOSB, 8, Prot::WRITE).is_err()
            || check_range(env.cpu, self.scratch + OFFSET, 8, Prot::WRITE).is_err()
            || check_range(env.cpu, self.buffer, CHUNK, Prot::WRITE).is_err()
            || env.cpu.write(self.scratch + IOSB, &[0; 8]).is_err()
            || env
                .cpu
                .write(
                    self.scratch + OFFSET,
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
            self.scratch + IOSB,
            self.buffer,
            CHUNK,
            self.scratch + OFFSET,
            0,
        ];
        Outcome::call(target, args, move |env, resumed| {
            let status = resumed.value();
            let reported = env
                .cpu
                .read_u32(self.scratch + IOSB + 4)
                .unwrap_or(u32::MAX);
            trace(
                env,
                "native_read",
                &[self.handle, self.bytes.len() as u32, CHUNK, reported],
                status,
            );
            if status == END_OF_FILE {
                let bytes = std::mem::take(&mut self.bytes);
                return self.close(env, Ok(Some(bytes)));
            }
            if status != 0 {
                if status == 0x103 {
                    // The provider may still own the writable IOSB and buffer.
                    // Stop without reusing them or pretending to cancel I/O.
                    return Outcome::Fail("loader: pending guest file I/O requires native asynchronous completion support".into());
                }
                return self.close(
                    env,
                    Err(if status as i32 >= 0 {
                        NOT_SUPPORTED
                    } else {
                        status
                    }),
                );
            }
            if check_range(env.cpu, self.scratch + IOSB, 8, Prot::READ).is_err() {
                return self.close(env, Err(ACCESS_VIOLATION));
            }
            let Ok(count) = env.cpu.read_u32(self.scratch + IOSB + 4) else {
                return self.close(env, Err(ACCESS_VIOLATION));
            };
            if count > CHUNK {
                warn_unavailable(
                    env,
                    "kernel32.dll!LoadLibrary",
                    "native file callback reported more bytes than the supplied buffer",
                );
                return self.close(env, Err(INVALID_PARAMETER));
            }
            if count == 0 {
                let bytes = std::mem::take(&mut self.bytes);
                return self.close(env, Ok(Some(bytes)));
            }
            if check_range(env.cpu, self.buffer, count, Prot::READ).is_err() {
                return self.close(env, Err(ACCESS_VIOLATION));
            }
            // A raw file larger than the guest address space cannot be a loaded
            // x86 image. Reserve fallibly; the hook never controls an unchecked
            // host allocation or pointer arithmetic.
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

pub(super) fn read(env: &mut Env<'_>, path: String, then: Completion) -> Outcome {
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
    let native = if let Some(extended) = path.strip_prefix(r"\\?\") {
        format!(r"\??\{extended}")
    } else if let Some(unc) = path.strip_prefix(r"\\") {
        format!(r"\??\UNC\{unc}")
    } else {
        format!(r"\??\{path}")
    };
    let units: Vec<_> = native.encode_utf16().collect();
    let length = units.len() * 2;
    if length > u16::MAX as usize - 2 {
        return then(env, Err(0xc0000033));
    }
    let buffer_offset = (PATH + length as u32 + 2 + 7) & !7;
    let Some(scratch) =
        env.ext
            .get_mut::<Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, 0, buffer_offset + CHUNK, true)
    else {
        return then(env, Err(NO_MEMORY));
    };
    let read = Read {
        scratch,
        buffer: scratch + buffer_offset,
        handle: 0,
        bytes: Vec::new(),
        then,
    };
    let initialized = (|| -> Result<(), MemFault> {
        env.cpu.write_u32(scratch + ATTRIBUTES, 24)?;
        env.cpu
            .write_u32(scratch + ATTRIBUTES + 8, scratch + NAME)?;
        env.cpu.write_u32(scratch + ATTRIBUTES + 12, 0x40)?;
        env.cpu
            .write(scratch + NAME, &(length as u16).to_le_bytes())?;
        env.cpu
            .write(scratch + NAME + 2, &(length as u16 + 2).to_le_bytes())?;
        env.cpu.write_u32(scratch + NAME + 4, scratch + PATH)?;
        let bytes: Vec<_> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        env.cpu.write(scratch + PATH, &bytes)
    })();
    if initialized.is_err() {
        return read.finish(env, Err(ACCESS_VIOLATION));
    }
    let args = if use_open {
        vec![
            scratch + HANDLE,
            0x80100000,
            scratch + ATTRIBUTES,
            scratch + IOSB,
            5,
            0x60,
        ]
    } else {
        vec![
            scratch + HANDLE,
            0x80100000,
            scratch + ATTRIBUTES,
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
            0xc0000034 | 0xc000003a | 0xc000000f => read.finish(env, Ok(None)),
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
                "loader: pending guest file open requires native asynchronous completion support"
                    .into(),
            ),
            status => read.finish(
                env,
                Err(if status as i32 >= 0 {
                    NOT_SUPPORTED
                } else {
                    status
                }),
            ),
        }
    })
}

#[cfg(test)]
mod tests;
