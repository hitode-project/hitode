//! Runtime helpers that game images link statically. Their exact bytes,
//! which carry no relocations, identify them in executable sections.

use adv_cpu::backend::Guest;
use adv_machine::{Conv, Ext, Sys};

use super::{Body, resume_original};

mod borland;
mod tvpgl;

fn helpers() -> impl Iterator<Item = (&'static str, Conv, &'static [u8])> {
    borland::HELPERS
        .iter()
        .map(|&(name, conv, bytes, _)| (name, conv, bytes))
        .chain(
            tvpgl::HELPERS
                .iter()
                .map(|&(name, conv, bytes, _)| (name, conv, bytes)),
        )
}

pub(super) fn bodies() -> impl Iterator<Item = Body> {
    borland::HELPERS
        .iter()
        .map(|&(name, conv, _, direct)| Body {
            name: name.into(),
            conv,
            body: None,
            handler: resume_original,
            direct: Some(direct),
        })
        .chain(tvpgl::HELPERS.iter().map(|&(name, conv, _, body)| Body {
            name: name.into(),
            conv,
            body: None,
            handler: resume_original,
            direct: Some(body),
        }))
}

/// Binds every helper found in the code at `start..start + len`. Each
/// copy gets its own thunk, named after its address.
pub(super) fn bind(cpu: &mut dyn Guest, sys: &mut Sys, ext: &mut Ext, start: u32, len: u32) {
    let mut code = vec![0; len as usize];
    if cpu.read(start, &mut code).is_err() {
        return;
    }
    for (name, _, bytes) in helpers() {
        for offset in memchr::memmem::find_iter(&code, bytes) {
            let entry = start + offset as u32;
            let thunk = format!("{name}@{entry:x}");
            if let Err(error) = super::bind_one(cpu, sys, ext, &thunk, entry, bytes.len() as u32) {
                log::warn!("{thunk}: no native body: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests;
