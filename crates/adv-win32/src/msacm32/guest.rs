//! Application-local ACM drivers execute their public DriverProc in the guest.
use super::*;
use adv_machine::Resume;
use adv_os::Heaps;

#[derive(Clone, Copy)]
struct Driver {
    alias: u32,
    proc: u32,
    module: u32,
    enabled: bool,
    private: u32,
    support: u32,
}
#[derive(Clone, Copy)]
struct Instance {
    driver: u32,
    proc: u32,
    private: u32,
}
#[derive(Default)]
struct Local {
    namespace: Option<Namespace>,
}
fn namespace(e: &mut Env<'_>) -> Result<Namespace, u32> {
    if e.ext.try_get::<Local>().is_none() {
        e.ext.insert(Local::default());
    }
    let local = e.ext.get_mut::<Local>();
    if local.namespace.is_none() {
        local.namespace = e.sys.objects.new_namespace_in(0xae00_0004, 0xaeff_fffc);
    }
    local.namespace.ok_or(NO_MEMORY)
}
fn driver(e: &Env<'_>, id: u32) -> Option<Driver> {
    let ns = e.ext.try_get::<Local>()?.namespace?;
    e.sys
        .objects
        .owned(e.sys.objects.lookup_alias(ns, id)?)
        .copied()
}
fn instance(e: &Env<'_>, id: u32) -> Option<Instance> {
    let ns = e.ext.try_get::<Local>()?.namespace?;
    e.sys
        .objects
        .owned(e.sys.objects.lookup_alias(ns, id)?)
        .copied()
}
fn remove(e: &mut Env<'_>, id: u32) {
    if let Some(ns) = e.ext.get::<Local>().namespace {
        e.sys.objects.close_alias(ns, id);
    }
}
fn scratch(e: &mut Env<'_>, words: &[u32]) -> Result<u32, u32> {
    let heap = e.ext.get_mut::<Heaps>();
    let address = heap
        .alloc(e.cpu, &mut e.sys.vm, 0, (words.len() * 4) as u32, true)
        .ok_or(NO_MEMORY)?;
    for (i, &value) in words.iter().enumerate() {
        if e.cpu.write_u32(address + i as u32 * 4, value).is_err() {
            free(e, address);
            return Err(INVALID_PARAMETER);
        }
    }
    Ok(address)
}
fn free(e: &mut Env<'_>, address: u32) {
    let heap = e.ext.get_mut::<Heaps>();
    heap.free(e.cpu, &mut e.sys.vm, heap.process_heap, address);
}
fn call(
    i: Instance,
    handle: u32,
    message: u32,
    a: u32,
    b: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    Outcome::call(
        i.proc,
        [i.private, handle, message, a, b],
        move |e, resume| match resume {
            Resume::Returned(value) => then(e, value),
            _ => Outcome::Return(NOT_POSSIBLE),
        },
    )
}
fn unload_driver(i: Instance, id: u32, opened: bool, code: u32) -> Outcome {
    let finish = move |_: &mut Env<'_>, _: u32| {
        call(i, id, 5, 0, 0, move |_, _| {
            call(i, id, 6, 0, 0, move |e, _| {
                remove(e, id);
                Outcome::Return(code)
            })
        })
    };
    if opened {
        call(i, id, 4, 0, 0, finish)
    } else {
        call(i, id, 5, 0, 0, move |_, _| {
            call(i, id, 6, 0, 0, move |e, _| {
                remove(e, id);
                Outcome::Return(code)
            })
        })
    }
}

fn add(e: &mut Env<'_>, output: u32, module: u32, proc: u32, priority: u32, flags: u32) -> Outcome {
    if flags != 3 {
        adv_abi::warn_unavailable(
            e,
            "acmDriverAdd",
            "only application-local function drivers are supported",
        );
        return Outcome::Return(NOT_SUPPORTED);
    }
    if priority != 0
        || output == 0
        || !e.cpu.accessible(output, 4, Prot::WRITE)
        || proc == 0
        || !e.cpu.accessible(proc, 1, Prot::EXEC)
    {
        return Outcome::Return(INVALID_PARAMETER);
    }
    let ns = match namespace(e) {
        Ok(ns) => ns,
        Err(code) => return Outcome::Return(code),
    };
    let Some(id) = e.sys.objects.insert_host_in(
        ns,
        Driver {
            alias: 0,
            proc,
            module,
            enabled: true,
            private: 0,
            support: 0,
        },
    ) else {
        return Outcome::Return(NO_MEMORY);
    };
    if let Some(identity) = e.sys.objects.lookup_alias::<Driver>(ns, id) {
        e.sys.objects.owned_mut(identity).unwrap().alias = id;
    }
    let i = Instance {
        driver: id,
        proc,
        private: 0,
    };
    call(i, id, 1, 0, 0, move |e, loaded| {
        if loaded == 0 {
            remove(e, id);
            return Outcome::Return(NOT_POSSIBLE);
        }
        call(i, id, 2, 0, 0, move |_, _| {
            call(i, id, 3, 0, 0, move |e, private| {
                if private == 0 {
                    return unload_driver(i, id, false, NOT_POSSIBLE);
                }
                let i = Instance { private, ..i };
                let mut words = vec![0; 450];
                words[0] = 1800;
                let desc = match scratch(e, &words) {
                    Ok(p) => p,
                    Err(code) => return unload_driver(i, id, true, code),
                };
                call(i, id, 0x600a, desc, 0, move |e, result| {
                    if result != 0 {
                        free(e, desc);
                        return unload_driver(i, id, true, result);
                    }
                    let support = e.cpu.read_u32(desc + 24).unwrap_or(0);
                    let count = e.cpu.read_u32(desc + 28).unwrap_or(0);
                    free(e, desc);
                    if count > 256 {
                        return unload_driver(i, id, true, NOT_POSSIBLE);
                    }
                    if let Some(identity) = e.sys.objects.lookup_alias::<Driver>(ns, id) {
                        let d = e.sys.objects.owned_mut(identity).unwrap();
                        d.private = private;
                        d.support = support;
                    }
                    discover_tags(e, i, id, output, 0, count)
                })
            })
        })
    })
}
fn discover_tags(
    e: &mut Env<'_>,
    i: Instance,
    id: u32,
    output: u32,
    index: u32,
    count: u32,
) -> Outcome {
    if index == count {
        return call(i, id, 8, 0, 0, move |_, _| {
            call(i, id, 0x600b, 0, 0, move |e, _| {
                if e.cpu.write_u32(output, id).is_err() {
                    unload_driver(i, id, true, INVALID_PARAMETER)
                } else {
                    Outcome::Return(0)
                }
            })
        });
    }
    let mut words = vec![0; 30];
    words[0] = 120;
    words[1] = index;
    let tag = match scratch(e, &words) {
        Ok(p) => p,
        Err(code) => return unload_driver(i, id, true, code),
    };
    call(i, id, 0x6019, tag, 0, move |e, result| {
        free(e, tag);
        if result != 0 {
            return unload_driver(i, id, true, result);
        }
        discover_tags(e, i, id, output, index + 1, count)
    })
}
#[win32(msacm32, name="acmDriverAddA", level=implement)]
fn add_a(
    e: &mut Env<'_>,
    output: GuestPtr<u32>,
    module: Handle,
    proc: u32,
    priority: u32,
    flags: u32,
) -> Outcome {
    add(e, output.addr(), module.0, proc, priority, flags)
}
#[win32(msacm32, name="acmDriverAddW", level=implement)]
fn add_w(
    e: &mut Env<'_>,
    output: GuestPtr<u32>,
    module: Handle,
    proc: u32,
    priority: u32,
    flags: u32,
) -> Outcome {
    add(e, output.addr(), module.0, proc, priority, flags)
}
#[win32(msacm32, name="acmDriverOpen", level=implement)]
fn open(e: &mut Env<'_>, output: GuestPtr<u32>, id: Handle, flags: u32) -> Outcome {
    if flags != 0 {
        return Outcome::Return(INVALID_FLAG);
    }
    if output.is_null() || !e.cpu.accessible(output.addr(), 4, Prot::WRITE) {
        return Outcome::Return(INVALID_PARAMETER);
    }
    let Some(d) = driver(e, id.0) else {
        return Outcome::Return(INVALID_HANDLE);
    };
    if !d.enabled {
        return Outcome::Return(NOT_POSSIBLE);
    }
    let desc = match scratch(
        e,
        &[
            36,
            u32::from_le_bytes(*b"audc"),
            0,
            0x04000000,
            0,
            0,
            0,
            0,
            0,
        ],
    ) {
        Ok(desc) => desc,
        Err(code) => return Outcome::Return(code),
    };
    let ns = e.ext.get::<Local>().namespace.unwrap();
    let Some(handle) = e.sys.objects.insert_host_in(
        ns,
        Instance {
            driver: id.0,
            proc: d.proc,
            private: 0,
        },
    ) else {
        free(e, desc);
        return Outcome::Return(NO_MEMORY);
    };
    call(
        Instance {
            driver: id.0,
            proc: d.proc,
            private: 0,
        },
        handle,
        3,
        0,
        desc,
        move |e, private| {
            let error = e.cpu.read_u32(desc + 20).unwrap_or(NOT_POSSIBLE);
            free(e, desc);
            if private == 0 {
                remove(e, handle);
                return Outcome::Return(if error == 0 { NOT_POSSIBLE } else { error });
            }
            let Some(identity) = e.sys.objects.lookup_alias::<Instance>(ns, handle) else {
                return Outcome::Return(INVALID_HANDLE);
            };
            e.sys.objects.owned_mut(identity).unwrap().private = private;
            if e.cpu.write_u32(output.addr(), handle).is_err() {
                return call(
                    Instance {
                        driver: id.0,
                        proc: d.proc,
                        private,
                    },
                    handle,
                    4,
                    0,
                    0,
                    move |e, _| {
                        remove(e, handle);
                        Outcome::Return(INVALID_PARAMETER)
                    },
                );
            }
            Outcome::Return(0)
        },
    )
}
#[win32(msacm32, name="acmDriverClose", level=implement)]
fn close(e: &mut Env<'_>, handle: Handle, flags: u32) -> Outcome {
    if flags != 0 {
        return Outcome::Return(INVALID_FLAG);
    }
    let Some(i) = instance(e, handle.0) else {
        return Outcome::Return(INVALID_HANDLE);
    };
    if e.sys
        .objects
        .all_hosts::<Stream>()
        .any(|(_, s)| s.driver_handle == handle.0)
    {
        return Outcome::Return(513);
    }
    call(i, handle.0, 4, 0, 0, move |e, _| {
        remove(e, handle.0);
        Outcome::Return(0)
    })
}
#[win32(msacm32, name="acmDriverRemove", level=implement)]
fn unregister(e: &mut Env<'_>, id: Handle, flags: u32) -> Outcome {
    if flags != 0 {
        return Outcome::Return(INVALID_FLAG);
    }
    let Some(d) = driver(e, id.0) else {
        return Outcome::Return(INVALID_HANDLE);
    };
    if e.sys
        .objects
        .all_hosts::<Instance>()
        .any(|(_, i)| i.driver == id.0)
    {
        return Outcome::Return(513);
    }
    let i = Instance {
        driver: id.0,
        proc: d.proc,
        private: d.private,
    };
    unload_driver(i, id.0, true, 0)
}
#[win32(msacm32, name="acmDriverMessage", level=implement)]
fn message(e: &mut Env<'_>, handle: Handle, message: u32, a: u32, b: u32) -> Outcome {
    let Some(i) = instance(e, handle.0) else {
        return Outcome::Return(INVALID_HANDLE);
    };
    call(i, handle.0, message, a, b, |_, result| {
        Outcome::Return(result)
    })
}
#[win32(msacm32, name="acmDriverID", level=state)]
fn id(e: &mut Env<'_>, handle: Handle, output: GuestPtr<u32>, flags: u32) -> u32 {
    if flags != 0 {
        return INVALID_FLAG;
    }
    let Some(i) = instance(e, handle.0).or_else(|| stream(e, handle.0).map(|s| s.instance)) else {
        return INVALID_HANDLE;
    };
    output
        .write(e.cpu, i.driver)
        .map_or(INVALID_PARAMETER, |_| 0)
}
pub(super) fn suggest(e: &mut Env<'_>, args: [u32; 5]) -> Outcome {
    let [handle, source, destination, size, flags] = args;
    if flags & !0xf0000 != 0 {
        return Outcome::Return(INVALID_FLAG);
    }
    if size < 18
        || !e.cpu.accessible(source, 18, Prot::READ)
        || !e.cpu.accessible(destination, size, Prot::RW)
    {
        return Outcome::Return(INVALID_PARAMETER);
    }
    let i = if handle != 0 {
        instance(e, handle)
    } else {
        enumeration(e, 0).first().and_then(|(id, _)| {
            driver(e, *id).map(|d| Instance {
                driver: *id,
                proc: d.proc,
                private: d.private,
            })
        })
    };
    let Some(i) = i else {
        return Outcome::Return(NOT_POSSIBLE);
    };
    let extra = u32::from(e.cpu.read_u16(source + 16).unwrap_or(0));
    if !e.cpu.accessible(source, 18 + extra, Prot::READ) {
        return Outcome::Return(INVALID_PARAMETER);
    }
    let temp = match scratch(e, &[24, flags, source, 18 + extra, destination, size]) {
        Ok(p) => p,
        Err(code) => return Outcome::Return(code),
    };
    call(
        i,
        if handle == 0 { i.driver } else { handle },
        0x601b,
        temp,
        0,
        move |e, result| {
            free(e, temp);
            Outcome::Return(result)
        },
    )
}
pub(super) fn register(r: &mut Registry) {
    for api in [
        add_a_ABI,
        add_w_ABI,
        open_ABI,
        close_ABI,
        unregister_ABI,
        message_ABI,
        id_ABI,
        module_ABI,
    ] {
        r.register(api);
    }
}

/// Snapshot local identities before callbacks; a callback may remove a later ID.
pub(super) fn enumeration(e: &Env<'_>, flags: u32) -> Vec<(u32, u32)> {
    if flags & 0x40000000 != 0 {
        return Vec::new();
    }
    let Some(ns) = e.ext.try_get::<Local>().and_then(|l| l.namespace) else {
        return Vec::new();
    };
    e.sys
        .objects
        .hosts_in::<Driver>(ns)
        .filter(|(_, d)| d.enabled || flags & 0x80000000 != 0)
        .map(|(_, d)| {
            (
                d.alias,
                d.support | 0x40000000 | if d.enabled { 0 } else { 0x80000000 },
            )
        })
        .collect()
}
pub(super) fn enumerate(
    e: &mut Env<'_>,
    callback: u32,
    context: u32,
    mut ids: Vec<(u32, u32)>,
    fallback: Option<(u32, u32)>,
) -> Outcome {
    if ids.is_empty() {
        return fallback.map_or(Outcome::Return(0), |(id, support)| {
            Outcome::call(callback, [id, context, support], |_, _| Outcome::Return(0))
        });
    }
    let (id, support) = ids.remove(0);
    if driver(e, id).is_none() {
        return enumerate(e, callback, context, ids, fallback);
    }
    Outcome::call(callback, [id, context, support], move |e, resume| {
        if resume.value() == 0 {
            Outcome::Return(0)
        } else {
            enumerate(e, callback, context, ids, fallback)
        }
    })
}
pub(super) fn details(
    e: &mut Env<'_>,
    id: u32,
    output: u32,
    flags: u32,
    wide: bool,
) -> Option<Outcome> {
    let d = driver(e, id)?;
    if flags != 0 {
        return Some(Outcome::Return(INVALID_FLAG));
    }
    let size = e.cpu.read_u32(output).unwrap_or(0);
    let available = size.min(if wide { 1800 } else { 920 });
    if output == 0 || size < 4 || !e.cpu.accessible(output, available, Prot::RW) {
        return Some(Outcome::Return(INVALID_PARAMETER));
    }
    let descriptor = match scratch(e, &{
        let mut w = vec![0; 450];
        w[0] = 1800;
        w
    }) {
        Ok(p) => p,
        Err(code) => return Some(Outcome::Return(code)),
    };
    Some(call(
        Instance {
            driver: id,
            proc: d.proc,
            private: d.private,
        },
        id,
        0x600a,
        descriptor,
        0,
        move |e, result| {
            let mut bytes = vec![0; 1800];
            let read = e.cpu.read(descriptor, &mut bytes);
            free(e, descriptor);
            if result != 0 {
                return Outcome::Return(result);
            }
            if read.is_err() {
                return Outcome::Return(INVALID_PARAMETER);
            }
            if !wide {
                let mut ansi = bytes[..40].to_vec();
                let codec = e.ext.get::<crate::Win32>().codec;
                let mut at = 40;
                for capacity in [32, 128, 80, 128, 512] {
                    let text: Vec<u16> = bytes[at..at + capacity * 2]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes(*b))
                        .take_while(|u| *u != 0)
                        .collect();
                    let text = codec.encode(&String::from_utf16_lossy(&text));
                    let mut field = vec![0; capacity];
                    let n = text.len().min(capacity - 1);
                    field[..n].copy_from_slice(&text[..n]);
                    ansi.extend(field);
                    at += capacity * 2;
                }
                bytes = ansi;
            }
            bytes[..4].copy_from_slice(&available.to_le_bytes());
            Outcome::Return(
                e.cpu
                    .write(output, &bytes[..available as usize])
                    .map_or(INVALID_PARAMETER, |_| 0),
            )
        },
    ))
}

#[derive(Clone)]
struct Stream {
    instance: Instance,
    driver_handle: u32,
    owned: bool,
    descriptor: u32,
    headers: BTreeMap<u32, Header>,
}
fn stream(e: &Env<'_>, h: u32) -> Option<Stream> {
    let ns = e.ext.try_get::<Local>()?.namespace?;
    e.sys
        .objects
        .owned(e.sys.objects.lookup_alias::<Stream>(ns, h)?)
        .cloned()
}
fn close_driver(
    _e: &mut Env<'_>,
    i: Instance,
    h: u32,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    call(i, h, 4, 0, 0, move |e, _| {
        remove(e, h);
        then(e)
    })
}
pub(super) fn open_stream(e: &mut Env<'_>, args: [u32; 8]) -> Outcome {
    let [
        output,
        had,
        source,
        destination,
        filter,
        callback,
        context,
        flags,
    ] = args;
    if flags & !7 != 0 {
        return Outcome::Return(INVALID_FLAG);
    }
    if flags & 2 != 0 || filter != 0 {
        return Outcome::Return(NOT_SUPPORTED);
    }
    if callback != 0
        || context != 0
        || !e.cpu.accessible(source, 18, Prot::READ)
        || !e.cpu.accessible(destination, 18, Prot::READ)
        || (flags & 1 == 0 && (output == 0 || !e.cpu.accessible(output, 4, Prot::WRITE)))
    {
        return Outcome::Return(INVALID_PARAMETER);
    }
    if had != 0 {
        let Some(i) = instance(e, had) else {
            return Outcome::Return(INVALID_HANDLE);
        };
        return open_with_instance(e, args, i, had, false);
    }
    let Some((id, _)) = enumeration(e, 0).first().copied() else {
        return Outcome::Return(NOT_POSSIBLE);
    };
    let temp = match scratch(e, &[0]) {
        Ok(p) => p,
        Err(code) => return Outcome::Return(code),
    };
    let outcome = open(e, GuestPtr::new(temp), Handle(id), 0);
    // Opening a driver is a continuation, just like the public API entry.
    match outcome {
        Outcome::Call {
            target,
            args: call_args,
            then,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args: call_args,
            stack,
            unwindable,
            then: Box::new(move |e, resume| {
                let next = then(e, resume);
                continue_open(e, next, args, temp)
            }),
        },
        other => continue_open(e, other, args, temp),
    }
}
fn continue_open(e: &mut Env<'_>, outcome: Outcome, args: [u32; 8], temp: u32) -> Outcome {
    match outcome {
        Outcome::Call {
            target,
            args: call_args,
            then,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args: call_args,
            stack,
            unwindable,
            then: Box::new(move |e, resume| {
                let next = then(e, resume);
                continue_open(e, next, args, temp)
            }),
        },
        Outcome::Return(0) => {
            let h = e.cpu.read_u32(temp).unwrap_or(0);
            free(e, temp);
            let Some(i) = instance(e, h) else {
                return Outcome::Return(INVALID_HANDLE);
            };
            open_with_instance(e, args, i, h, true)
        }
        other => {
            free(e, temp);
            other
        }
    }
}
fn open_with_instance(
    e: &mut Env<'_>,
    args: [u32; 8],
    i: Instance,
    had: u32,
    owned: bool,
) -> Outcome {
    let [
        output,
        _,
        source,
        destination,
        filter,
        callback,
        context,
        flags,
    ] = args;
    let descriptor = match scratch(
        e,
        &[
            40,
            source,
            destination,
            filter,
            callback,
            context,
            flags,
            0,
            0,
            0,
        ],
    ) {
        Ok(p) => p,
        Err(code) => {
            return if owned {
                close_driver(e, i, had, move |_| Outcome::Return(code))
            } else {
                Outcome::Return(code)
            };
        }
    };
    let ns = e.ext.get::<Local>().namespace.unwrap();
    let h = if flags & 1 != 0 {
        0
    } else {
        let Some(h) = e.sys.objects.insert_host_in(
            ns,
            Stream {
                instance: i,
                driver_handle: had,
                owned,
                descriptor,
                headers: BTreeMap::new(),
            },
        ) else {
            free(e, descriptor);
            return if owned {
                close_driver(e, i, had, |_| Outcome::Return(NO_MEMORY))
            } else {
                Outcome::Return(NO_MEMORY)
            };
        };
        h
    };
    let _ = e.cpu.write_u32(descriptor + 36, h);
    call(i, had, 0x604c, descriptor, 0, move |e, result| {
        if result != 0 || flags & 1 != 0 {
            if h != 0 {
                remove(e, h);
            }
            free(e, descriptor);
            return if owned {
                close_driver(e, i, had, move |_| Outcome::Return(result))
            } else {
                Outcome::Return(result)
            };
        }
        if e.cpu.write_u32(output, h).is_err() {
            return finish_close(e, h, INVALID_PARAMETER);
        }
        Outcome::Return(0)
    })
}
fn finish_close(e: &mut Env<'_>, h: u32, result: u32) -> Outcome {
    let Some(s) = stream(e, h) else {
        return Outcome::Return(INVALID_HANDLE);
    };
    call(
        s.instance,
        s.driver_handle,
        0x604d,
        s.descriptor,
        0,
        move |e, closed| {
            if closed != 0 {
                return Outcome::Return(closed);
            }
            remove(e, h);
            free(e, s.descriptor);
            if s.owned {
                close_driver(e, s.instance, s.driver_handle, move |_| {
                    Outcome::Return(result)
                })
            } else {
                Outcome::Return(result)
            }
        },
    )
}
pub(super) fn stream_call(
    e: &mut Env<'_>,
    h: u32,
    message: u32,
    arg: u32,
    flags: u32,
) -> Option<Outcome> {
    let s = stream(e, h)?;
    if message == 0x604d {
        return Some(if flags != 0 {
            Outcome::Return(INVALID_FLAG)
        } else {
            finish_close(e, h, 0)
        });
    }
    if message == 0x604e {
        // arg is an output pointer; input size is the second public argument.
        let input = e.arg(1);
        if flags > 1 || input == 0 || arg == 0 || !e.cpu.accessible(arg, 4, Prot::WRITE) {
            return Some(Outcome::Return(INVALID_PARAMETER));
        }
        let temp = match scratch(
            e,
            &[
                16,
                flags,
                if flags == 0 { input } else { 0 },
                if flags == 1 { input } else { 0 },
            ],
        ) {
            Ok(p) => p,
            Err(code) => return Some(Outcome::Return(code)),
        };
        return Some(call(
            s.instance,
            s.driver_handle,
            message,
            s.descriptor,
            temp,
            move |e, result| {
                let value = e
                    .cpu
                    .read_u32(temp + if flags == 0 { 12 } else { 8 })
                    .unwrap_or(0);
                free(e, temp);
                if result != 0 {
                    Outcome::Return(result)
                } else {
                    Outcome::Return(e.cpu.write_u32(arg, value).map_or(INVALID_PARAMETER, |_| 0))
                }
            },
        ));
    }
    if message == 0x6050 {
        return Some(if flags != 0 {
            Outcome::Return(INVALID_FLAG)
        } else {
            call(
                s.instance,
                s.driver_handle,
                message,
                s.descriptor,
                0,
                |_, v| Outcome::Return(v),
            )
        });
    }
    let header = match Header::read(e.cpu, arg) {
        Ok(h) => h,
        Err(code) => return Some(Outcome::Return(code)),
    };
    if (message == 0x604f && flags & !(4 | 16 | 32) != 0) || (message != 0x604f && flags != 0) {
        return Some(Outcome::Return(INVALID_FLAG));
    }
    if !header.buffers_valid(e.cpu) {
        return Some(Outcome::Return(INVALID_PARAMETER));
    }
    if message == 0x6051 && s.headers.contains_key(&arg) {
        return Some(Outcome::Return(0));
    }
    if message != 0x6051 {
        let Some(prepared) = s.headers.get(&arg) else {
            return Some(Outcome::Return(UNPREPARED));
        };
        if header.src != prepared.src
            || header.dst != prepared.dst
            || header.src_len > prepared.src_len
            || header.dst_len > prepared.dst_len
        {
            return Some(Outcome::Return(INVALID_PARAMETER));
        }
    }
    if message == 0x604f {
        let _ = e.cpu.write_u32(arg + 44, flags);
        let _ = e.cpu.write_u32(arg + 20, 0);
        let _ = e.cpu.write_u32(arg + 36, 0);
    }
    Some(call(
        s.instance,
        s.driver_handle,
        message,
        s.descriptor,
        arg,
        move |e, result| {
            // Preparing buffers is optional for synchronous ACM drivers.
            if result != 0 && !(matches!(message, 0x6051 | 0x6052) && result == NOT_SUPPORTED) {
                return Outcome::Return(result);
            }
            let ns = e.ext.get::<Local>().namespace.unwrap();
            let Some(identity) = e.sys.objects.lookup_alias::<Stream>(ns, h) else {
                return Outcome::Return(INVALID_HANDLE);
            };
            let stream = e.sys.objects.owned_mut(identity).unwrap();
            let status = match message {
                0x6051 => {
                    stream.headers.insert(arg, header);
                    header.status | PREPARED
                }
                0x6052 => {
                    stream.headers.remove(&arg);
                    header.status & !PREPARED
                }
                _ => header.status | PREPARED | DONE,
            };
            Outcome::Return(
                e.cpu
                    .write_u32(arg + 4, status)
                    .map_or(INVALID_PARAMETER, |_| 0),
            )
        },
    ))
}
#[win32(winmm, name="GetDriverModuleHandle", level=state)]
fn module(e: &mut Env<'_>, h: Handle) -> Handle {
    let id = instance(e, h.0).map_or(h.0, |i| i.driver);
    Handle(driver(e, id).map_or(0, |d| d.module))
}
