use super::*;
use adv_cpu::backend::Prot;

#[derive(Default)]
pub(super) struct Sound {
    current: Option<(u32, u32, bool)>,
}

pub(super) fn pump(env: &mut Env<'_>) {
    if let Some((handle, event, asynchronous)) = env.ext.get::<Sound>().current
        && wave::sound_finished(env, handle)
    {
        wave::discard_sound(env, handle);
        env.ext.get_mut::<Sound>().current = None;
        env.sys.set_event(event);
        if asynchronous {
            env.sys.objects.close(event);
        }
    }
}
fn stop(env: &mut Env<'_>) {
    if let Some((handle, event, asynchronous)) = env.ext.get_mut::<Sound>().current.take() {
        wave::discard_sound(env, handle);
        env.sys.set_event(event);
        if asynchronous {
            env.sys.objects.close(event);
        }
    }
}
type Then = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;
fn guest(env: &mut Env<'_>, dll: &str, name: &str, args: Vec<u32>, then: Then) -> Outcome {
    let target = adv_nt::process::module_base(env.cpu, env.sys.peb, dll)
        .and_then(|b| adv_nt::process::module_export(env.cpu, b, name));
    match target {
        Some(target) => Outcome::call(target, args, move |env, result| then(env, result.value())),
        None => Outcome::Fail(format!("WinMM sound import missing: {dll}!{name}")),
    }
}
fn wide(env: &Env<'_>, p: u32) -> Result<String, String> {
    let mut units = Vec::new();
    let mut at = p;
    loop {
        let n = env.cpu.read_u16(at).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        units.push(n);
        at += 2;
    }
    Ok(String::from_utf16_lossy(&units))
}
fn put(env: &mut Env<'_>, p: u32, s: &str) -> Result<(), String> {
    let bytes: Vec<_> = s
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    env.cpu.write(p, &bytes).map_err(|e| e.to_string())
}
struct Resolve {
    scratch: u32,
    flags: u32,
    name: u32,
    module: u32,
    default: bool,
}
impl Resolve {
    fn end(self, env: &mut Env<'_>, bytes: Option<Vec<u8>>) -> Outcome {
        if let Err(e) = env.sys.vm.free(env.cpu, self.scratch) {
            return Outcome::Fail(e.to_string());
        }
        match bytes {
            Some(bytes) => play(env, bytes, self.flags),
            None => Outcome::Return(u32::from(self.flags & 1 != 0)),
        }
    }
}
pub(super) fn call(env: &mut Env<'_>, name: &str, a: Vec<u32>) -> Outcome {
    pump(env);
    let (pointer, module, flags) = if name.starts_with("snd") {
        (a[0], 0, a[1] & 0x17_001f)
    } else {
        (a[0], a[1], a[2])
    };
    if flags & 16 != 0 && env.ext.get::<Sound>().current.is_some() {
        return Outcome::Return(0);
    }
    stop(env);
    if pointer == 0 || flags & 0x40 != 0 {
        return Outcome::Return(1);
    }
    if flags & 0x40004 != 0x40004 && flags & 4 != 0 {
        return memory(env, pointer, flags);
    }
    let scratch = match env.sys.vm.alloc(env.cpu, 8192, Prot::RW) {
        Ok(p) => p,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let mut r = Resolve {
        scratch,
        flags,
        name: pointer,
        module,
        default: false,
    };
    if flags & 0x110000 == 0x110000 {
        let alias = match pointer {
            0x2a53 => "SystemAsterisk",
            0x4453 => "SystemDefault",
            0x2153 => "SystemExclamation",
            0x4553 => "SystemExit",
            0x4853 => "SystemHand",
            0x3f53 => "SystemQuestion",
            0x5353 => "SystemStart",
            0x5753 => "SystemWelcome",
            _ => return r.end(env, None),
        };
        if let Err(e) = put(env, scratch, alias) {
            return Outcome::Fail(e);
        }
        r.name = scratch;
        return resolve(env, r);
    }
    if name.ends_with('W') || (flags & 0x40004 == 0x40004 && pointer >> 16 == 0) {
        return resolve(env, r);
    }
    guest(
        env,
        "kernel32.dll",
        "MultiByteToWideChar",
        vec![0, 0, pointer, u32::MAX, scratch, 512],
        Box::new(move |env, n| {
            if n == 0 {
                return r.end(env, None);
            }
            r.name = scratch;
            resolve(env, r)
        }),
    )
}
fn resolve(env: &mut Env<'_>, r: Resolve) -> Outcome {
    if r.flags & 0x40004 == 0x40004 {
        let ty = r.scratch + 1024;
        if let Err(e) = put(env, ty, "WAVE") {
            return Outcome::Fail(e);
        }
        return guest(
            env,
            "kernel32.dll",
            "FindResourceW",
            vec![r.module, r.name, ty],
            Box::new(move |env, res| {
                if res == 0 {
                    return r.end(env, None);
                }
                guest(
                    env,
                    "kernel32.dll",
                    "LoadResource",
                    vec![r.module, res],
                    Box::new(move |env, res| {
                        guest(
                            env,
                            "kernel32.dll",
                            "LockResource",
                            vec![res],
                            Box::new(move |env, p| {
                                let bytes = memory_bytes(env, p);
                                r.end(env, bytes)
                            }),
                        )
                    }),
                )
            }),
        );
    }
    if r.flags & 0x10000 != 0 {
        profile(env, r, 0, true)
    } else if r.flags & 0x20000 != 0 {
        let name = r.name;
        open_file(env, r, name, 0, true)
    } else {
        profile(env, r, 0, false)
    }
}
fn memory_bytes(env: &Env<'_>, pointer: u32) -> Option<Vec<u8>> {
    let length = read(env, pointer + 4).ok()?;
    let mut bytes = vec![0; length as usize + 8];
    env.cpu.read(pointer, &mut bytes).ok()?;
    Some(bytes)
}
fn memory(env: &mut Env<'_>, pointer: u32, flags: u32) -> Outcome {
    match memory_bytes(env, pointer) {
        Some(bytes) => play(env, bytes, flags),
        None => Outcome::Return(u32::from(flags & 1 != 0)),
    }
}
fn play(env: &mut Env<'_>, bytes: Vec<u8>, flags: u32) -> Outcome {
    let failure = Outcome::Return(u32::from(flags & 1 != 0));
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return failure;
    }
    let mut at = 12;
    let mut format = None;
    let mut data = None;
    while at + 8 <= bytes.len() {
        let length = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let start = at + 8;
        if length > bytes.len() - start {
            return failure;
        }
        match &bytes[at..at + 4] {
            b"fmt " => format = Some((start, length)),
            b"data" => data = Some((start, length)),
            _ => {}
        }
        at = start + length + (length & 1);
    }
    let (Some((fmt_start, fmt_length)), Some((start, length))) = (format, data) else {
        return failure;
    };
    let fmt = &bytes[fmt_start..fmt_start + fmt_length];
    match wave::Format::parse(fmt) {
        Some(format) => play_pcm(env, format, &bytes[start..start + length], flags),
        None => convert(env, fmt, &bytes[start..start + length], flags),
    }
}
fn play_pcm(env: &mut Env<'_>, format: wave::Format, bytes: &[u8], flags: u32) -> Outcome {
    let length = bytes.len() / usize::from(format.align) * usize::from(format.align);
    let handle = wave::play_sound(env, format, &bytes[..length], flags & 9 == 9);
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    env.ext.get_mut::<Sound>().current = Some((handle, event, flags & 1 != 0));
    env.ext.get_mut::<Stats>().sounds += 1;
    if flags & 1 != 0 {
        Outcome::Return(1)
    } else {
        Outcome::block(
            Wait::Object {
                handle: event,
                deadline: None,
            },
            move |env, _| {
                env.sys.objects.close(event);
                Outcome::Return(1)
            },
        )
    }
}
// Stages preserve Wine's profile / registry / file precedence.
fn profile(env: &mut Env<'_>, r: Resolve, stage: u32, alias: bool) -> Outcome {
    let section = r.scratch + 1024;
    let default = r.scratch + 1040;
    let buffer = r.scratch + 2048;
    if let Err(e) = put(env, section, "Sounds").and_then(|()| put(env, default, "Default")) {
        return Outcome::Fail(e);
    }
    let name = if stage == 1 { default } else { r.name };
    guest(
        env,
        "kernel32.dll",
        "GetProfileStringW",
        vec![section, name, section + 12, buffer, 128],
        Box::new(move |env, n| {
            if n != 0 {
                if let Ok(path) = wide(env, buffer) {
                    let path = path.split(',').next().unwrap();
                    if let Err(e) = put(env, buffer, path) {
                        return Outcome::Fail(e);
                    }
                }
                return open_file(env, r, buffer, if alias { 1 } else { 3 }, false);
            }
            if stage == 0 && alias && r.flags & 2 == 0 {
                return profile(env, r, 1, alias);
            }
            registry(env, r, alias)
        }),
    )
}
fn registry(env: &mut Env<'_>, r: Resolve, alias: bool) -> Outcome {
    if r.flags & 0x80 != 0 {
        let buffer = r.scratch + 4096;
        guest(
            env,
            "kernel32.dll",
            "GetModuleFileNameW",
            vec![0, buffer, 256],
            Box::new(move |env, n| {
                let app = if n > 0 && n < 256 {
                    wide(env, buffer).ok().and_then(|s| {
                        s.rsplit('\\')
                            .next()
                            .map(|s| s.rsplit_once('.').map_or(s, |(s, _)| s).to_owned())
                    })
                } else {
                    None
                };
                match app {
                    Some(app) => registry_app(env, r, alias, &app),
                    None => after_alias(env, r, alias),
                }
            }),
        )
    } else {
        registry_app(env, r, alias, ".Default")
    }
}
fn registry_app(env: &mut Env<'_>, r: Resolve, alias: bool, app: &str) -> Outcome {
    let name = match wide(env, r.name) {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e),
    };
    let path = format!("AppEvents\\Schemes\\Apps\\{app}\\{name}");
    registry_key(env, r, alias, path, false)
}
fn registry_key(
    env: &mut Env<'_>,
    r: Resolve,
    alias: bool,
    path: String,
    current: bool,
) -> Outcome {
    let buffer = r.scratch + 4096;
    let output = r.scratch + 6144;
    let full = format!("{path}\\{}", if current { ".Current" } else { ".Default" });
    if let Err(e) = put(env, buffer, &full) {
        return Outcome::Fail(e);
    }
    guest(
        env,
        "advapi32.dll",
        "RegOpenKeyW",
        vec![0x8000_0001, buffer, output],
        Box::new(move |env, status| {
            if status != 0 {
                return if current {
                    after_alias(env, r, alias)
                } else {
                    registry_key(env, r, alias, path, true)
                };
            }
            let key = match read(env, output) {
                Ok(h) => h,
                Err(e) => return Outcome::Fail(e),
            };
            let count = output + 4;
            if let Err(e) = write(env, count, 256) {
                return Outcome::Fail(e);
            }
            guest(
                env,
                "advapi32.dll",
                "RegQueryValueExW",
                vec![key, 0, 0, 0, buffer, count],
                Box::new(move |env, status| {
                    guest(
                        env,
                        "advapi32.dll",
                        "RegCloseKey",
                        vec![key],
                        Box::new(move |env, _| {
                            if status == 0 && env.cpu.read_u16(buffer).is_ok_and(|v| v != 0) {
                                open_file(env, r, buffer, if alias { 2 } else { 4 }, false)
                            } else {
                                after_alias(env, r, alias)
                            }
                        }),
                    )
                }),
            )
        }),
    )
}
fn after_alias(env: &mut Env<'_>, r: Resolve, alias: bool) -> Outcome {
    if r.default {
        return r.end(env, None);
    }
    let name = r.name;
    open_file(env, r, name, if alias { 0 } else { 5 }, true)
}
fn open_file(env: &mut Env<'_>, r: Resolve, pointer: u32, stage: u32, search: bool) -> Outcome {
    let open = match imported(env, ORIGINAL, "mmioOpenW") {
        Ok(i) => i,
        Err(e) => return Outcome::Fail(e),
    };
    open.call(env, [pointer, 0, 0x10020], move |env, result| {
        let handle = result.value();
        if handle == 0 {
            if search {
                let extension = r.scratch + 1040;
                let found = r.scratch + 3072;
                if let Err(e) = put(env, extension, ".wav") {
                    return Outcome::Fail(e);
                }
                return guest(
                    env,
                    "kernel32.dll",
                    "SearchPathW",
                    vec![0, pointer, extension, 256, found, 0],
                    Box::new(move |env, n| {
                        if n != 0 && n < 256 {
                            open_file(env, r, found, stage, false)
                        } else {
                            file_miss(env, r, stage)
                        }
                    }),
                );
            }
            return file_miss(env, r, stage);
        }
        let seek = match imported(env, ORIGINAL, "mmioSeek") {
            Ok(i) => i,
            Err(e) => return Outcome::Fail(e),
        };
        seek.call(env, [handle, 0, 2], move |env, length| {
            let length = length.value();
            if length == u32::MAX || length == 0 {
                return close_file(env, r, handle, 0, 0);
            }
            let buffer = match env.sys.vm.alloc(env.cpu, length, Prot::RW) {
                Ok(p) => p,
                Err(e) => return Outcome::Fail(e.to_string()),
            };
            let seek = match imported(env, ORIGINAL, "mmioSeek") {
                Ok(i) => i,
                Err(e) => return Outcome::Fail(e),
            };
            seek.call(env, [handle, 0, 0], move |env, _| {
                let read = match imported(env, ORIGINAL, "mmioRead") {
                    Ok(i) => i,
                    Err(e) => return Outcome::Fail(e),
                };
                read.call(env, [handle, buffer, length], move |env, result| {
                    close_file(
                        env,
                        r,
                        handle,
                        buffer,
                        if result.value() == length { length } else { 0 },
                    )
                })
            })
        })
    })
}
fn file_miss(env: &mut Env<'_>, r: Resolve, stage: u32) -> Outcome {
    match stage {
        1 => registry(env, r, true),
        2 => after_alias(env, r, true),
        3 => registry(env, r, false),
        4 => after_alias(env, r, false),
        5 if r.flags & 2 == 0 => {
            let mut r = r;
            r.default = true;
            profile(env, r, 0, true)
        }
        _ => r.end(env, None),
    }
}
fn close_file(env: &mut Env<'_>, r: Resolve, handle: u32, buffer: u32, length: u32) -> Outcome {
    let close = match imported(env, ORIGINAL, "mmioClose") {
        Ok(i) => i,
        Err(e) => return Outcome::Fail(e),
    };
    close.call(env, [handle, 0], move |env, _| {
        let mut bytes = vec![0; length as usize];
        if buffer != 0 {
            if let Err(e) = env.cpu.read(buffer, &mut bytes) {
                return Outcome::Fail(e.to_string());
            }
            if let Err(e) = env.sys.vm.free(env.cpu, buffer) {
                return Outcome::Fail(e.to_string());
            }
        }
        r.end(env, Some(bytes))
    })
}

// Wine's ACM performs the conversion; the resulting PCM uses the native queue.
fn convert(env: &mut Env<'_>, fmt: &[u8], bytes: &[u8], flags: u32) -> Outcome {
    if fmt.len() < 16 {
        return Outcome::Return(u32::from(flags & 1 != 0));
    }
    let mut target = [0; 18];
    target[..16].copy_from_slice(&fmt[..16]);
    target[..2].copy_from_slice(&1u16.to_le_bytes());
    target[14..16].copy_from_slice(&16u16.to_le_bytes());
    let align = u16::from_le_bytes([target[2], target[3]]).wrapping_mul(2);
    target[12..14].copy_from_slice(&align.to_le_bytes());
    let rate = u32::from_le_bytes(target[4..8].try_into().unwrap());
    target[8..12].copy_from_slice(&rate.wrapping_mul(u32::from(align)).to_le_bytes());
    let Some(format) = wave::Format::parse(&target) else {
        return Outcome::Return(u32::from(flags & 1 != 0));
    };
    let length = bytes.len() as u32;
    let source_length = fmt.len().max(18) as u32;
    let base = match env
        .sys
        .vm
        .alloc(env.cpu, 256 + source_length + length, Prot::RW)
    {
        Ok(p) => p,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let source = base + 256 + source_length;
    if let Err(e) = env
        .cpu
        .write(base + 16, &target)
        .and_then(|()| env.cpu.write(base + 256, fmt))
        .and_then(|()| env.cpu.write(source, bytes))
    {
        return Outcome::Fail(e.to_string());
    }
    guest(
        env,
        "msacm32.dll",
        "acmStreamOpen",
        vec![base, 0, base + 256, base + 16, 0, 0, 0, 0],
        Box::new(move |env, status| {
            if status != 0 {
                if let Err(e) = env.sys.vm.free(env.cpu, base) {
                    return Outcome::Fail(e.to_string());
                }
                return Outcome::Return(u32::from(flags & 1 != 0));
            }
            let stream = match read(env, base) {
                Ok(h) => h,
                Err(e) => return Outcome::Fail(e),
            };
            guest(
                env,
                "msacm32.dll",
                "acmStreamSize",
                vec![stream, length, base + 4, 0],
                Box::new(move |env, status| {
                    if status != 0 {
                        return converted(env, stream, base, 0, None, format, flags);
                    }
                    let size = match read(env, base + 4) {
                        Ok(n) => n,
                        Err(e) => return Outcome::Fail(e),
                    };
                    let output = match env.sys.vm.alloc(env.cpu, size, Prot::RW) {
                        Ok(p) => p,
                        Err(e) => return Outcome::Fail(e.to_string()),
                    };
                    let header = base + 64;
                    if let Err(e) = adv_drv::write_words(
                        env,
                        header,
                        &[84, 0, 0, source, length, 0, 0, output, size, 0, 0],
                    ) {
                        return Outcome::Fail(e);
                    }
                    guest(
                        env,
                        "msacm32.dll",
                        "acmStreamPrepareHeader",
                        vec![stream, header, 0],
                        Box::new(move |env, status| {
                            if status != 0 {
                                return converted(env, stream, base, output, None, format, flags);
                            }
                            guest(
                                env,
                                "msacm32.dll",
                                "acmStreamConvert",
                                vec![stream, header, 0],
                                Box::new(move |env, status| {
                                    let bytes = if status == 0 {
                                        let length = match read(env, header + 36) {
                                            Ok(n) => n,
                                            Err(e) => return Outcome::Fail(e),
                                        };
                                        let mut bytes = vec![0; length as usize];
                                        if let Err(e) = env.cpu.read(output, &mut bytes) {
                                            return Outcome::Fail(e.to_string());
                                        }
                                        Some(bytes)
                                    } else {
                                        None
                                    };
                                    guest(
                                        env,
                                        "msacm32.dll",
                                        "acmStreamUnprepareHeader",
                                        vec![stream, header, 0],
                                        Box::new(move |env, _| {
                                            converted(
                                                env, stream, base, output, bytes, format, flags,
                                            )
                                        }),
                                    )
                                }),
                            )
                        }),
                    )
                }),
            )
        }),
    )
}
fn converted(
    env: &mut Env<'_>,
    stream: u32,
    base: u32,
    output: u32,
    bytes: Option<Vec<u8>>,
    format: wave::Format,
    flags: u32,
) -> Outcome {
    guest(
        env,
        "msacm32.dll",
        "acmStreamClose",
        vec![stream, 0],
        Box::new(move |env, _| {
            if let Err(e) = env.sys.vm.free(env.cpu, base) {
                return Outcome::Fail(e.to_string());
            }
            if output != 0
                && let Err(e) = env.sys.vm.free(env.cpu, output)
            {
                return Outcome::Fail(e.to_string());
            }
            match bytes {
                Some(bytes) => play_pcm(env, format, &bytes, flags),
                None => Outcome::Return(u32::from(flags & 1 != 0)),
            }
        }),
    )
}
