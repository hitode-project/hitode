//! DirectDraw 8-bit palettes use the indexed surface store's color table.
use super::{ComState, INVALID, Kind, State};
use crate::argument::{Arguments, CallError};
#[cfg(feature = "gpu")]
use adv_abi::{Api, ImplementationLevel, Registry, com::QueryError};
use adv_abi::{Env, Outcome};
#[cfg(feature = "gpu")]
use adv_cpu::backend::Backend;
use adv_device::surface::{Presenter, SurfaceStore, Surfaces};
use adv_machine::Objects;
#[cfg(feature = "gpu")]
use adv_machine::{Machine, thunk::Conv};

const OUT_OF_MEMORY: u32 = 0x8007000e;
const IID: [u8; 16] = [
    0x84, 0xdb, 0x14, 0x6c, 0x33, 0xa7, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];

#[cfg(feature = "gpu")]
const METHODS: [(&str, u8); 7] = [
    ("IDirectDrawPalette::QueryInterface", 3),
    ("IDirectDrawPalette::AddRef", 1),
    ("IDirectDrawPalette::Release", 1),
    ("IDirectDrawPalette::GetCaps", 2),
    ("IDirectDrawPalette::GetEntries", 5),
    ("IDirectDrawPalette::Initialize", 4),
    ("IDirectDrawPalette::SetEntries", 5),
];

pub(super) struct Palette {
    #[cfg(feature = "gpu")]
    flags: u32,
    entries: [[u8; 4]; 256],
}

#[cfg(feature = "gpu")]
pub(in crate::ddraw) fn register(registry: &mut Registry) {
    for (slot, (name, argc)) in METHODS.into_iter().enumerate() {
        registry.register_with_data(
            Api {
                module: "ddraw.dll",
                name,
                conv: Conv::Stdcall { argc },
                handler: dispatch,
                level: ImplementationLevel::Implement,
            },
            slot as u64,
        );
    }
}

#[cfg(feature = "gpu")]
pub(in crate::ddraw) fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), String> {
    let (state, store) = machine.ext.get2_installed_mut::<State, SurfaceStore>();
    let mut memory = (state.provider.memory)(&mut machine.cpu);
    let table = store.alloc(METHODS.len() as u32 * 4, &mut memory)?;
    for (slot, (name, _)) in METHODS.into_iter().enumerate() {
        let thunk = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, "ddraw.dll", name)
            .map_err(|e| e.to_string())?;
        let mut memory = (state.provider.memory)(&mut machine.cpu);
        memory
            .arg_put_u32(table + slot as u32 * 4, thunk)
            .map_err(|_| "unable to write palette vtable")?;
    }
    state.draw.palette_table = table;
    Ok(())
}

pub(super) fn surface_api(env: &mut Env<'_>, method: &str, args: &[u32]) -> Outcome {
    outcome(env, |state, store, objects, memory| {
        let draw = &mut state.draw;
        let com = draw.com.unwrap();
        match method {
            "CreatePalette" => {
                memory.arg_put_u32(args[3], 0)?;
                if !matches!(com.get::<ComState>(objects, args[0]), Some(owner) if matches!(owner.kind, Kind::Device) && owner.initialized)
                {
                    return Ok(INVALID);
                }
                if args[4] != 0 {
                    return Ok(0x80040110);
                }
                let flags = args[1];
                if flags & 4 == 0 || flags & !(4 | 0x10 | 0x40) != 0 {
                    return Ok(INVALID);
                }
                let entries = bytemuck::pod_read_unaligned(&memory.arg_read(args[2], 1024)?);
                let palette = Palette {
                    #[cfg(feature = "gpu")]
                    flags,
                    entries,
                };
                let pointer = store.alloc(16, memory)?;
                memory.arg_put_u32(pointer, draw.palette_table)?;
                if com.create(objects, pointer, IID, palette).is_none() {
                    store.release(pointer, 16);
                    return Ok(OUT_OF_MEMORY);
                }
                memory.arg_put_u32(args[3], pointer)?;
            }
            "GetPalette" => {
                let Some(sid) = draw.surface_id(objects, args[0]) else {
                    return Ok(INVALID);
                };
                let palette = draw.surface_palettes.get(&sid).copied().unwrap_or(0);
                memory.arg_put_u32(args[1], palette)?;
                if palette == 0 {
                    return Ok(0x8876023c);
                }
                com.add_ref::<Palette>(objects, palette);
            }
            "SetPalette" => {
                let Some(sid) = draw.surface_id(objects, args[0]) else {
                    return Ok(INVALID);
                };
                if !store[sid].format.indexed() {
                    return Ok(INVALID);
                }
                let pointer = args[1];
                let colors = if pointer == 0 {
                    vec![[0, 0, 0, 255]; 256]
                } else {
                    let Some(palette) = com.get::<Palette>(objects, pointer) else {
                        return Ok(INVALID);
                    };
                    convert(&palette.entries)
                };
                store.set_palette(sid, 0, &colors, memory)?;
                let previous = if pointer == 0 {
                    draw.surface_palettes.remove(&sid)
                } else {
                    com.add_ref::<Palette>(objects, pointer);
                    draw.surface_palettes.insert(sid, pointer)
                };
                if let Some(previous) = previous {
                    release(com, objects, store, previous);
                }
            }
            _ => unreachable!(),
        }
        Ok(0)
    })
}

#[cfg(feature = "gpu")]
fn dispatch(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data as usize;
    let mut args = [0; 5];
    for (n, word) in args.iter_mut().take(METHODS[slot].1 as usize).enumerate() {
        *word = env.arg(n as u32);
    }
    outcome(env, |state, store, objects, memory| {
        let draw = &mut state.draw;
        let com = draw.com.unwrap();
        let this = args[0];
        match slot {
            0 => {
                memory.arg_put_u32(args[2], 0)?;
                let iid = memory.arg_read(args[1], 16)?.as_ref().try_into().unwrap();
                match com.query::<Palette>(objects, this, iid) {
                    Ok(pointer) => memory.arg_put_u32(args[2], pointer)?,
                    Err(QueryError::NoInterface) => return Ok(0x80004002),
                    Err(QueryError::ReferenceLimit) => return Ok(OUT_OF_MEMORY),
                    Err(QueryError::InvalidObject) => return Ok(INVALID),
                }
            }
            1 => return Ok(com.add_ref::<Palette>(objects, this).unwrap_or(0)),
            2 => return Ok(release(com, objects, store, this)),
            3..=6 => {
                let Some(palette) = com.get_mut::<Palette>(objects, this) else {
                    return Ok(INVALID);
                };
                match slot {
                    3 => memory.arg_put_u32(args[1], palette.flags)?,
                    5 => return Ok(super::ALREADY_INITIALIZED),
                    4 | 6 => {
                        let (start, count) = (args[2] as usize, args[3] as usize);
                        if args[1] != 0 || start + count > 256 {
                            return Ok(INVALID);
                        }
                        let entries = &mut palette.entries[start..start + count];
                        if slot == 4 {
                            memory.arg_write(args[4], bytemuck::cast_slice(entries))?;
                        } else {
                            let data = memory.arg_read(args[4], count * 4)?;
                            entries.copy_from_slice(bytemuck::cast_slice(&data));
                            let colors = convert(entries);
                            for (&sid, _) in
                                draw.surface_palettes.iter().filter(|&(_, &p)| p == this)
                            {
                                store.set_palette(sid, start as u32, &colors, memory)?;
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            _ => unreachable!(),
        }
        Ok(0)
    })
}

fn convert(entries: &[[u8; 4]]) -> Vec<[u8; 4]> {
    entries.iter().map(|&[r, g, b, _]| [b, g, r, 255]).collect()
}

fn outcome(
    env: &mut Env<'_>,
    call: impl FnOnce(
        &mut State,
        &mut SurfaceStore,
        &mut Objects,
        &mut dyn adv_device::surface::SurfaceMemory,
    ) -> Result<u32, CallError>,
) -> Outcome {
    let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
    let mut memory = (state.provider.memory)(env.cpu);
    match call(state, store, &mut env.sys.objects, &mut memory) {
        Ok(hr) => Outcome::Return(hr),
        Err(error) => error.outcome(env),
    }
}

pub(in crate::ddraw) fn release(
    com: adv_abi::com::Com,
    objects: &mut Objects,
    store: &mut Surfaces<impl Presenter>,
    pointer: u32,
) -> u32 {
    let Some(released) = com.release::<Palette>(objects, pointer) else {
        return 0;
    };
    if released.destroyed.is_some() {
        store.release(pointer, 16);
    }
    released.references
}
