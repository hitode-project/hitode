//! Digital-video MCI control uses the registered DirectShow graph and its owners.
//! No second decoder, mixer, renderer, or guest execution loop is introduced.
use super::*;
use adv_machine::{Conv, Namespace, Wait};
use adv_os::Heaps;

const WORKER: &str = "__pal_mci_notify";
const GRAPH: u32 = 128;
const CONTROL: u32 = 140;
const VIDEO: u32 = 144;
const EVENTS: u32 = 148;
const SEEKING: u32 = 152;
const BASIC: u32 = 156;
const PARAMS: u32 = 192;
const PATH: u32 = 256;
const UNSUPPORTED: u32 = 274;
const FILE_NOT_FOUND: u32 = 275;
const INTERNAL: u32 = 289;

#[derive(Clone, Copy)]
struct Device {
    block: u32,
    window: u32,
    target: u32,
    mode: u32,
    format: u32,
    notification: u32,
    generation: u32,
}
#[derive(Default)]
struct Devices {
    namespace: Option<Namespace>,
}
fn device(e: &Env<'_>, id: u32) -> Option<Device> {
    let ns = e.ext.try_get::<Devices>()?.namespace?;
    e.sys
        .objects
        .owned(e.sys.objects.lookup_alias(ns, id)?)
        .copied()
}
fn change(e: &mut Env<'_>, id: u32, f: impl FnOnce(&mut Device)) {
    let Some(ns) = e.ext.get::<Devices>().namespace else {
        return;
    };
    if let Some(key) = e.sys.objects.lookup_alias::<Device>(ns, id) {
        f(e.sys.objects.owned_mut(key).unwrap());
    }
}
pub(super) fn alloc(e: &mut Env<'_>, size: u32) -> Option<u32> {
    e.ext
        .get_mut::<Heaps>()
        .alloc(e.cpu, &mut e.sys.vm, 0, size, true)
}
pub(super) fn free(e: &mut Env<'_>, p: u32) {
    let heaps = e.ext.get_mut::<Heaps>();
    heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, p);
}
fn api(
    e: &mut Env<'_>,
    name: &str,
    args: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let module = if name == "CoCreateInstance" {
        "ole32.dll"
    } else {
        "user32.dll"
    };
    let target = match e.sys.thunks.publish(e.cpu, module, name) {
        Ok(p) => p,
        Err(_) => return Outcome::Return(UNSUPPORTED),
    };
    Outcome::call(target, args, move |e, r| then(e, r.value()))
}
fn com(
    e: &mut Env<'_>,
    pointer: u32,
    slot: u32,
    args: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let target = e
        .cpu
        .read_u32(pointer)
        .and_then(|v| e.cpu.read_u32(v + slot * 4));
    let Ok(target) = target else {
        return Outcome::Return(INTERNAL);
    };
    let mut values = vec![pointer];
    values.extend(args);
    Outcome::call(target, values, move |e, r| then(e, r.value()))
}
fn face(e: &Env<'_>, block: u32, offset: u32) -> u32 {
    e.cpu.read_u32(block + offset).unwrap_or(0)
}
fn method(
    e: &mut Env<'_>,
    block: u32,
    offset: u32,
    slot: u32,
    args: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    com(e, face(e, block, offset), slot, args, then)
}
fn fail_open(e: &mut Env<'_>, block: u32, code: u32) -> Outcome {
    release(
        e,
        block,
        vec![128, 132, 136, 140, 144, 148, 152, 156, 160],
        move |e| {
            free(e, block);
            Outcome::Return(code)
        },
    )
}
fn release(
    e: &mut Env<'_>,
    block: u32,
    mut offsets: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    if offsets.is_empty() {
        return then(e);
    }
    let at = offsets.remove(0);
    let p = face(e, block, at);
    if p == 0 {
        return release(e, block, offsets, then);
    }
    let _ = e.cpu.write_u32(block + at, 0);
    com(e, p, 2, vec![], move |e, _| {
        release(e, block, offsets, then)
    })
}
fn text(e: &Env<'_>, p: u32, wide: bool) -> Option<String> {
    if wide {
        WideStr(GuestPtr::new(p))
            .read(e.cpu, 32768)
            .ok()
            .map(|s| String::from_utf16_lossy(&s))
    } else {
        AnsiStr(GuestPtr::new(p))
            .read(e.cpu, 32768)
            .ok()
            .map(|s| e.ext.get::<crate::Win32>().codec.decode(&s))
    }
}
pub(super) fn send(
    e: &mut Env<'_>,
    id: u32,
    message: u32,
    flags: u32,
    p: u32,
    wide: bool,
) -> Option<Outcome> {
    if message == MCI_OPEN {
        if !adv_device::media::Decoder::available() || p == 0 || !e.cpu.accessible(p, 20, Prot::RW)
        {
            return None;
        }
        if flags & 0x2000 == 0 || flags & 0x200 == 0 || flags & 0x800 != 0 {
            return None;
        }
        let kind = e.cpu.read_u32(p + 8).ok()?;
        let supported = if flags & 0x1000 != 0 {
            kind == 520
        } else {
            text(e, kind, wide).is_some_and(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "avivideo" | "digitalvideo" | "mpegvideo"
                )
            })
        };
        if !supported {
            return None;
        }
        let Some(path) = e.cpu.read_u32(p + 12).ok().and_then(|s| text(e, s, wide)) else {
            return Some(Outcome::Return(MISSING_STRING_ARGUMENT));
        };
        return Some(open(e, p, flags, path));
    }
    let d = device(e, id)?;
    Some(command(e, id, d, message, flags, p))
}
fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let mut b = [0; 16];
    b[..4].copy_from_slice(&d1.to_le_bytes());
    b[4..6].copy_from_slice(&d2.to_le_bytes());
    b[6..8].copy_from_slice(&d3.to_le_bytes());
    b[8..].copy_from_slice(&d4);
    b
}
fn open(e: &mut Env<'_>, output: u32, flags: u32, path: String) -> Outcome {
    if flags & !(0x2000 | 0x1000 | 0x200 | 0x20000 | 2) != 0 {
        return Outcome::Return(UNSUPPORTED);
    }
    if flags & 0x20000 != 0 && !e.cpu.accessible(output, 28, Prot::RW) {
        return Outcome::Return(MISSING_PARAMETER);
    }
    let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let Some(block) = alloc(e, PATH + path.len() as u32 * 2) else {
        return Outcome::Return(UNSUPPORTED);
    };
    let quartz = [0xb0, 0x3a, 0, 0x20, 0xaf, 0x0b, 0xa7, 0x70];
    for (at, id) in [
        (
            0,
            guid(
                0xe436ebb3,
                0x524f,
                0x11ce,
                [0x9f, 0x53, 0, 0x20, 0xaf, 0x0b, 0xa7, 0x70],
            ),
        ),
        (16, guid(0x56a868a9, 0x0ad4, 0x11ce, quartz)),
        (32, guid(0x56a868b1, 0x0ad4, 0x11ce, quartz)),
        (48, guid(0x56a868b4, 0x0ad4, 0x11ce, quartz)),
        (64, guid(0x56a868c0, 0x0ad4, 0x11ce, quartz)),
        (
            80,
            guid(
                0x36b73880,
                0xc2c8,
                0x11cf,
                [0x8b, 0x46, 0, 0x80, 0x5f, 0x6c, 0xef, 0x60],
            ),
        ),
        (96, guid(0x56a868b5, 0x0ad4, 0x11ce, quartz)),
    ] {
        let _ = e.cpu.write(block + at, &id);
    }
    let bytes: Vec<u8> = path.into_iter().flat_map(u16::to_le_bytes).collect();
    let _ = e.cpu.write(block + PATH, &bytes);
    api(
        e,
        "CoCreateInstance",
        vec![block, 0, 1, block + 16, block + GRAPH],
        move |e, hr| {
            if hr & 0x80000000 != 0 {
                return fail_open(e, block, UNSUPPORTED);
            }
            method(
                e,
                block,
                GRAPH,
                14,
                vec![block + PATH, 0, block + 132],
                move |e, hr| {
                    if hr & 0x80000000 != 0 {
                        return fail_open(e, block, FILE_NOT_FOUND);
                    }
                    method(e, block, 132, 10, vec![block + 160], move |e, hr| {
                        if hr & 0x80000000 != 0 {
                            return fail_open(e, block, INTERNAL);
                        }
                        method(e, block, 160, 3, vec![1, block + 136, 0], move |e, hr| {
                            if hr != 0 {
                                return fail_open(e, block, INTERNAL);
                            }
                            method(
                                e,
                                block,
                                GRAPH,
                                12,
                                vec![face(e, block, 136)],
                                move |e, hr| {
                                    if hr & 0x80000000 != 0 {
                                        return fail_open(e, block, UNSUPPORTED);
                                    }
                                    interfaces(e, block, 0, output, flags)
                                },
                            )
                        })
                    })
                },
            )
        },
    )
}
fn interfaces(e: &mut Env<'_>, block: u32, index: usize, output: u32, flags: u32) -> Outcome {
    let pairs = [
        (32, CONTROL),
        (48, VIDEO),
        (64, EVENTS),
        (80, SEEKING),
        (96, BASIC),
    ];
    if let Some(&(guid, out)) = pairs.get(index) {
        return method(
            e,
            block,
            GRAPH,
            0,
            vec![block + guid, block + out],
            move |e, hr| {
                if hr & 0x80000000 != 0 {
                    fail_open(e, block, UNSUPPORTED)
                } else {
                    interfaces(e, block, index + 1, output, flags)
                }
            },
        );
    }
    method(e, block, VIDEO, 13, vec![0], move |e, hr| {
        if hr & 0x80000000 != 0 {
            return fail_open(e, block, UNSUPPORTED);
        }
        method(
            e,
            block,
            BASIC,
            34,
            vec![block + 224, block + 228],
            move |e, hr| {
                let width = e.cpu.read_u32(block + 224).unwrap_or(0);
                let height = e.cpu.read_u32(block + 228).unwrap_or(0);
                if hr & 0x80000000 != 0 || width == 0 || height == 0 {
                    return fail_open(e, block, UNSUPPORTED);
                }
                // MCI owns a real USER window; clients can reparent it with ordinary APIs.
                let name = block + 192;
                let _ = e.cpu.write(name, b"STATIC\0");
                let parent = if flags & 0x20000 != 0 {
                    e.cpu.read_u32(output + 24).unwrap_or(0)
                } else {
                    0
                };
                api(
                    e,
                    "CreateWindowExA",
                    vec![
                        0,
                        name,
                        name,
                        if parent != 0 { 0x40000000 } else { 0x80000000 },
                        0,
                        0,
                        width,
                        height,
                        parent,
                        0,
                        0,
                        0,
                    ],
                    move |e, window| {
                        if window == 0 {
                            return fail_open(e, block, INTERNAL);
                        }
                        method(e, block, VIDEO, 29, vec![window], move |e, hr| {
                            if hr & 0x80000000 != 0 {
                                return api(e, "DestroyWindow", vec![window], move |e, _| {
                                    fail_open(e, block, UNSUPPORTED)
                                });
                            }
                            if e.ext.try_get::<Devices>().is_none() {
                                e.ext.insert(Devices::default());
                            }
                            let ns = {
                                let ds = e.ext.get_mut::<Devices>();
                                if ds.namespace.is_none() {
                                    ds.namespace = e.sys.objects.new_namespace_in(4, 65532);
                                }
                                ds.namespace
                            };
                            let Some(ns) = ns else {
                                return api(e, "DestroyWindow", vec![window], move |e, _| {
                                    fail_open(e, block, INTERNAL)
                                });
                            };
                            let Some(id) = e.sys.objects.insert_host_in(
                                ns,
                                Device {
                                    block,
                                    window,
                                    target: window,
                                    mode: 525,
                                    format: 0,
                                    notification: 0,
                                    generation: 0,
                                },
                            ) else {
                                return api(e, "DestroyWindow", vec![window], move |e, _| {
                                    fail_open(e, block, INTERNAL)
                                });
                            };
                            let _ = e.cpu.write_u32(output + 4, id);
                            release(e, block, vec![132, 136, 160], |_| Outcome::Return(0))
                        })
                    },
                )
            },
        )
    })
}
fn command(e: &mut Env<'_>, id: u32, d: Device, message: u32, flags: u32, p: u32) -> Outcome {
    if message == 0x804 {
        if flags & !2 != 0 {
            return Outcome::Return(UNSUPPORTED);
        }
        change(e, id, |d| {
            d.generation = d.generation.wrapping_add(1);
            d.notification = 0;
        });
        let close = move |e: &mut Env<'_>| {
            method(e, d.block, CONTROL, 9, vec![], move |e, _| {
                method(e, d.block, VIDEO, 19, vec![0], move |e, _| {
                    api(e, "DestroyWindow", vec![d.window], move |e, _| {
                        let ns = e.ext.get::<Devices>().namespace.unwrap();
                        e.sys.objects.close_alias(ns, id);
                        release(
                            e,
                            d.block,
                            vec![GRAPH, CONTROL, VIDEO, EVENTS, SEEKING, BASIC],
                            move |e| {
                                free(e, d.block);
                                Outcome::Return(0)
                            },
                        )
                    })
                })
            })
        };
        return if d.notification != 0 {
            api(
                e,
                "PostMessageA",
                vec![d.notification, 0x3b9, 4, id],
                move |e, _| close(e),
            )
        } else {
            close(e)
        };
    }
    if p == 0 {
        return Outcome::Return(NULL_PARAMETER_BLOCK);
    }
    let size = if matches!(message, 0x808 | 0x809 | 0x855) {
        4
    } else if matches!(message, 0x814 | 0x80b | 0x841) {
        16
    } else if matches!(message, 0x842 | 0x843) {
        20
    } else {
        12
    };
    if !e.cpu.accessible(p, size, Prot::RW) {
        return Outcome::Return(MISSING_PARAMETER);
    }
    let value = e.cpu.read_u32(p + 4).unwrap_or(0);
    match message {
        0x841 if flags & 0x10000 != 0 => {
            if flags & !(0x10000 | 0x40000 | 0x100000 | 2) != 0 {
                return Outcome::Return(UNSUPPORTED);
            }
            method(e, d.block, VIDEO, 29, vec![value], move |e, hr| {
                if hr & 0x80000000 != 0 {
                    return Outcome::Return(UNSUPPORTED);
                }
                change(e, id, |d| d.target = value);
                if flags & 0x40000 != 0 {
                    let show = e.cpu.read_u32(p + 8).unwrap_or(0) & 0xffff;
                    return api(e, "ShowWindow", vec![value, show], move |e, _| {
                        method(
                            e,
                            d.block,
                            VIDEO,
                            19,
                            vec![if show == 0 { 0 } else { u32::MAX }],
                            |_, hr| {
                                Outcome::Return(if hr & 0x80000000 == 0 { 0 } else { UNSUPPORTED })
                            },
                        )
                    });
                }
                Outcome::Return(0)
            })
        }
        0x80d if flags & 0x400 != 0 => {
            if flags & !(0x400 | 2) != 0 {
                return Outcome::Return(UNSUPPORTED);
            }
            if value != 0 {
                Outcome::Return(UNSUPPORTED)
            } else {
                change(e, id, |d| d.format = value);
                Outcome::Return(0)
            }
        }
        0x806 => {
            if flags & !(1 | 2 | 4 | 8) != 0 {
                return Outcome::Return(UNSUPPORTED);
            }
            let notify = if flags & 1 != 0 {
                e.cpu.read_u32(p).unwrap_or(0)
            } else {
                0
            };
            let start = move |e: &mut Env<'_>| {
                api(e, "ShowWindow", vec![d.target, 5], move |e, _| {
                    method(e, d.block, VIDEO, 19, vec![u32::MAX], move |e, hr| {
                        if hr & 0x80000000 != 0 {
                            return Outcome::Return(UNSUPPORTED);
                        }
                        method(e, d.block, CONTROL, 7, vec![], move |e, hr| {
                            if hr & 0x80000000 != 0 {
                                return Outcome::Return(UNSUPPORTED);
                            }
                            let previous = device(e, id).map_or(0, |d| d.notification);
                            change(e, id, |d| {
                                d.mode = 526;
                                d.notification = notify;
                                d.generation = d.generation.wrapping_add(1);
                            });
                            let generation = device(e, id).unwrap().generation;
                            if previous != 0 {
                                return api(
                                    e,
                                    "PostMessageA",
                                    vec![
                                        previous,
                                        0x3b9,
                                        if flags & (4 | 8) != 0 { 4 } else { 2 },
                                        id,
                                    ],
                                    move |e, _| monitor(e, id, generation, flags & 2 != 0),
                                );
                            }
                            monitor(e, id, generation, flags & 2 != 0)
                        })
                    })
                })
            };
            if flags & (4 | 8) != 0 {
                let from = u64::from(e.cpu.read_u32(p + 4).unwrap_or(0)) * 10000;
                let to = u64::from(e.cpu.read_u32(p + 8).unwrap_or(0)) * 10000;
                for (at, value) in [(208, from), (216, to)] {
                    let _ = e.cpu.write_u32(d.block + at, value as u32);
                    let _ = e.cpu.write_u32(d.block + at + 4, (value >> 32) as u32);
                }
                method(
                    e,
                    d.block,
                    SEEKING,
                    14,
                    vec![
                        d.block + 208,
                        u32::from(flags & 4 != 0),
                        d.block + 216,
                        u32::from(flags & 8 != 0),
                    ],
                    move |e, hr| {
                        if hr & 0x80000000 != 0 {
                            Outcome::Return(UNSUPPORTED)
                        } else {
                            start(e)
                        }
                    },
                )
            } else {
                start(e)
            }
        }
        0x808 | 0x809 | 0x855 => {
            if flags & !2 != 0 {
                return Outcome::Return(UNSUPPORTED);
            }
            let (slot, mode) = match message {
                0x808 => (9, 525),
                0x809 => (8, 529),
                _ => (7, 526),
            };
            method(e, d.block, CONTROL, slot, vec![], move |e, hr| {
                if hr & 0x80000000 != 0 {
                    return Outcome::Return(UNSUPPORTED);
                }
                change(e, id, |d| {
                    d.mode = mode;
                    if message == 0x808 {
                        d.generation = d.generation.wrapping_add(1);
                        d.notification = 0;
                    }
                });
                if message == 0x808 && d.notification != 0 {
                    // STOP aborts a pending PLAY notification, even when STOP
                    // itself has no callback. Retire the old monitor first.
                    return api(
                        e,
                        "PostMessageA",
                        vec![d.notification, 0x3b9, 4, id],
                        |_, _| Outcome::Return(0),
                    );
                }
                Outcome::Return(0)
            })
        }
        0x814 => {
            if flags & 0x100 == 0 || flags & !(0x100 | 2) != 0 {
                return Outcome::Return(UNSUPPORTED);
            }
            let item = e.cpu.read_u32(p + 8).unwrap_or(0);
            match item {
                4 | 5 | 6 | 7 | 0x4001 => {
                    let value = match item {
                        4 => d.mode,
                        5 | 7 => 1,
                        6 => d.format,
                        _ => d.window,
                    };
                    Outcome::Return(
                        e.cpu
                            .write_u32(p + 4, value)
                            .map_or(MISSING_PARAMETER, |_| 0),
                    )
                }
                1 | 2 => {
                    let slot = if item == 1 { 10 } else { 12 };
                    method(
                        e,
                        d.block,
                        SEEKING,
                        slot,
                        vec![d.block + 224],
                        move |e, hr| {
                            if hr & 0x80000000 != 0 {
                                return Outcome::Return(UNSUPPORTED);
                            }
                            let time = (u64::from(e.cpu.read_u32(d.block + 224).unwrap_or(0))
                                | (u64::from(e.cpu.read_u32(d.block + 228).unwrap_or(0)) << 32))
                                / 10000;
                            Outcome::Return(
                                e.cpu
                                    .write_u32(p + 4, time.min(u64::from(u32::MAX)) as u32)
                                    .map_or(MISSING_PARAMETER, |_| 0),
                            )
                        },
                    )
                }
                _ => Outcome::Return(UNSUPPORTED),
            }
        }
        0x843 | 0x842 => {
            if flags & 0x40000 == 0 || flags & !(0x40000 | 0x10000 | 2) != 0 {
                return Outcome::Return(UNSUPPORTED);
            }
            if message == 0x842 {
                let mut r = [0; 4];
                for (i, v) in r.iter_mut().enumerate() {
                    *v = e.cpu.read_u32(p + 4 + i as u32 * 4).unwrap_or(0);
                }
                method(e, d.block, VIDEO, 39, r.to_vec(), |_, hr| {
                    Outcome::Return(if hr & 0x80000000 == 0 { 0 } else { UNSUPPORTED })
                })
            } else {
                method(
                    e,
                    d.block,
                    VIDEO,
                    40,
                    vec![p + 4, p + 8, p + 12, p + 16],
                    |_, hr| Outcome::Return(if hr & 0x80000000 == 0 { 0 } else { UNSUPPORTED }),
                )
            }
        }
        _ => {
            adv_abi::warn_unavailable(
                e,
                "mciSendCommand",
                "digital-video command or flags are unavailable",
            );
            Outcome::Return(UNSUPPORTED)
        }
    }
}
fn monitor(e: &mut Env<'_>, id: u32, generation: u32, waiting: bool) -> Outcome {
    if waiting {
        return poll(e, id, generation, true);
    }
    let entry = match e.sys.thunks.publish(e.cpu, "winmm.dll", WORKER) {
        Ok(p) => p,
        Err(_) => return Outcome::Return(INTERNAL),
    };
    Outcome::Return(
        if e.sys
            .spawn(e.cpu, "MCI notification", entry, &[id, generation], 65536)
            .is_ok()
        {
            0
        } else {
            INTERNAL
        },
    )
}
fn poll(e: &mut Env<'_>, id: u32, generation: u32, waiting: bool) -> Outcome {
    let Some(d) = device(e, id).filter(|d| d.generation == generation) else {
        return if waiting {
            Outcome::Return(0)
        } else {
            Outcome::ExitThread(0)
        };
    };
    method(
        e,
        d.block,
        EVENTS,
        8,
        vec![
            d.block + PARAMS,
            d.block + PARAMS + 4,
            d.block + PARAMS + 8,
            0,
        ],
        move |e, hr| {
            if device(e, id).is_none_or(|current| current.generation != generation) {
                return if waiting {
                    Outcome::Return(0)
                } else {
                    Outcome::ExitThread(0)
                };
            }
            if hr == 0 {
                let event = e.cpu.read_u32(d.block + PARAMS).unwrap_or(0);
                if matches!(event, 1 | 2 | 3) {
                    let reason = if event == 1 {
                        1
                    } else if event == 2 {
                        4
                    } else {
                        8
                    };
                    change(e, id, |d| {
                        d.mode = 525;
                        d.notification = 0;
                    });
                    if d.notification != 0 {
                        return api(
                            e,
                            "PostMessageA",
                            vec![d.notification, 0x3b9, reason, id],
                            move |_, _| {
                                if waiting {
                                    Outcome::Return(0)
                                } else {
                                    Outcome::ExitThread(0)
                                }
                            },
                        );
                    }
                    return if waiting {
                        Outcome::Return(if event == 3 { INTERNAL } else { 0 })
                    } else {
                        Outcome::ExitThread(0)
                    };
                }
            }
            Outcome::block(Wait::Until(e.sys.clock.now_us() + 10000), move |e, _| {
                poll(e, id, generation, waiting)
            })
        },
    )
}
fn worker(e: &mut Env<'_>) -> Outcome {
    poll(e, e.arg(0), e.arg(1), false)
}
pub(super) fn register(r: &mut Registry) {
    r.add("winmm.dll", WORKER, Conv::Stdcall { argc: 2 }, worker);
}
