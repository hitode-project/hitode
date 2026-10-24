//! Static codec libraries on Wine's image-mapping path. Admission and the
//! library ABIs follow the built-in stack; decoding belongs to adv-device.
//! Redirects leave image bytes unchanged and declined calls use trampolines.
#[cfg(all(test, any(target_vendor = "apple", target_os = "android")))]
use adv_wineserver as _;
use std::collections::BTreeMap;

use adv_abi::code_signature::ExecutableRange;
use adv_cpu::backend::{Guest, Prot, asm::Asm};
use adv_machine::{Env, Ext, HostFn, Outcome, Sys, layout::PAGE};

mod mdct;
mod theora;

pub use mdct::{BUNDLED_SIGNATURES, Stats};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    Guest,
    Verify,
    #[default]
    Native,
}

struct Images {
    mode: Mode,
    mapped: BTreeMap<u32, u32>,
}

/// Start binding before Wine maps the program and its libraries.
pub fn install(ext: &mut Ext, mode: Mode) {
    ext.insert(Images {
        mode,
        mapped: BTreeMap::new(),
    });
    mdct::install(ext, mode);
    theora::install(ext, mode);
    ext.insert(adv_nt::section::ImageBinding {
        mapped: |env, base, pe| {
            let ranges: Vec<_> = pe
                .sections
                .iter()
                .filter_map(|s| {
                    if s.characteristics & 0x2000_0000 == 0 {
                        return None;
                    }
                    let start = base.checked_add(s.virtual_address)?;
                    let len = s
                        .virtual_size
                        .max(s.raw_size)
                        .min(pe.size_of_image.checked_sub(s.virtual_address)?);
                    (len != 0).then_some(ExecutableRange { start, len })
                })
                .collect();
            note_image(env, base, pe.size_of_image, &ranges)
        },
        unmapped: forget_image,
    });
}

pub fn mode(ext: &Ext) -> Mode {
    ext.try_get::<Images>()
        .map_or(Mode::Native, |images| images.mode)
}

pub fn stats(ext: &Ext) -> Option<Stats> {
    mdct::stats(ext)
}

/// The session's resolver delegates codec thunk names here.
pub fn host_fn(module: &str, name: &str) -> Option<HostFn> {
    mdct::host_fn(module, name).or_else(|| theora::host_fn(module, name))
}

fn note_image(env: &mut Env<'_>, base: u32, size: u32, ranges: &[ExecutableRange]) {
    if mode(env.ext) == Mode::Guest
        || env
            .ext
            .try_get::<adv_nt::identity::Images>()
            .is_some_and(|images| images.0.contains_key(&base))
        || env.ext.get::<Images>().mapped.contains_key(&base)
    {
        return;
    }
    env.ext.get_mut::<Images>().mapped.insert(base, size);
    if let Err(error) = mdct::bind(env, base, size, ranges).and_then(|()| theora::bind(env, ranges))
    {
        mdct::rejected(env.ext, error);
    }
}

fn forget_image(env: &mut Env<'_>, base: u32) {
    if let Some(size) = env.ext.get_mut::<Images>().mapped.remove(&base) {
        env.sys.redirects.retain(|&entry, _| {
            entry < base || u64::from(entry) >= u64::from(base) + u64::from(size)
        });
        mdct::forget(env.ext, base);
        theora::forget(env.ext, base, size);
    }
}

struct Entry {
    entry: u32,
    original: u32,
    thunk: u32,
    trampoline: Vec<u8>,
}

/// Keep the same reviewed movable prefix without modifying the image.
fn entry(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    module: &str,
    name: &str,
    address: u32,
    prefix: usize,
    bytes: &[u8],
) -> Result<Entry, String> {
    sys.thunks
        .publish(cpu, module, name)
        .map_err(|e| e.to_string())?;
    let index = sys
        .thunks
        .published_index(module, name)
        .ok_or("no codec thunk")?;
    let original = sys
        .vm
        .alloc(cpu, PAGE, Prot::RX)
        .map_err(|e| e.to_string())?;
    let mut trampoline = Asm::new(original);
    trampoline.bytes.extend_from_slice(&bytes[..prefix]);
    trampoline.jmp(address + prefix as u32);
    cpu.write(original, &trampoline.bytes)
        .map_err(|e| e.to_string())?;
    Ok(Entry {
        entry: address,
        original,
        thunk: index,
        trampoline: trampoline.bytes,
    })
}

fn redirect(sys: &mut Sys, entry: &Entry, bytes: &[u8]) {
    sys.redirects.insert(
        entry.entry,
        (adv_machine::layout::thunk_addr(entry.thunk), bytes.into()),
    );
}

fn original(env: &mut Env<'_>, target: u32) -> Outcome {
    match env.checkpoint() {
        Ok(mut cp) => {
            cp.state.eip = target;
            Outcome::ResumeExecution(cp)
        }
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

#[cfg(test)]
#[path = "../../tests/codec/images.rs"]
mod tests;
