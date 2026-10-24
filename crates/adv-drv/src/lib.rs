//! Wine's driver modules on the Wine userland (issue #58), as host modules
//! over the host devices. The user driver places windows and gives them
//! surfaces in the process surface store; win32u loads it as Wine's Mac or X11
//! driver.
#![cfg(unix)]

use std::path::Path;

use adv_abi::{Env, Outcome, Registry};
use adv_cpu::backend::{Backend, Guest, Prot};
use adv_device::surface::MemoryView;
use adv_machine::Machine;
use adv_machine::layout::PAGE;

pub mod audio;
pub mod media;
pub mod user;

/// What the drivers take from the assembly.
#[derive(Clone, Copy)]
pub struct Provider {
    /// Guest memory as the surface store reads it.
    pub memory: for<'a> fn(&'a mut (dyn Guest + 'a)) -> MemoryView<'a, dyn Guest + 'a>,
    /// A presentation ended frame `serial`; it may end the run.
    pub completed: fn(&mut Env<'_>, u64) -> Option<Outcome>,
}

pub fn register(registry: &mut Registry) {
    audio::register(registry);
    media::register(registry);
    user::register(registry);
}

/// Install the drivers over the surface store and put their images in
/// `prefix`. Call before the process starts with [`user::MODULE`] as its
/// graphics driver.
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    prefix: &Path,
    provider: Provider,
) -> Result<(), String> {
    machine.ext.insert(provider);
    machine
        .ext
        .insert(adv_nt::process::Desktop(user::start_desktop));
    let image = user::image(machine)?;
    adv_unix::builtin::install(prefix, &image).map_err(|e| e.to_string())?;
    audio::install(machine, prefix)?;
    Ok(())
}

/// Free 64-byte blocks of guest memory.
#[derive(Default)]
struct Slots(Vec<u32>);

const SLOT: u32 = 64;

/// A 64-byte block of guest memory for a call's in and out parameters.
pub fn slot(env: &mut Env<'_>) -> Result<u32, String> {
    if let Some(slot) = env.ext.get_or_default::<Slots>().0.pop() {
        return Ok(slot);
    }
    let page = env
        .sys
        .vm
        .alloc(env.cpu, PAGE, Prot::RW)
        .map_err(|e| e.to_string())?;
    let slots = &mut env.ext.get_mut::<Slots>().0;
    slots.extend((1..PAGE / SLOT).map(|index| page + index * SLOT));
    Ok(page)
}

/// Give back a block from [`slot`].
pub fn free_slot(env: &mut Env<'_>, slot: u32) {
    env.ext.get_or_default::<Slots>().0.push(slot);
}

pub fn write_words(env: &mut Env<'_>, at: u32, words: &[u32]) -> Result<(), String> {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    env.cpu.write(at, &bytes).map_err(|e| e.to_string())
}
