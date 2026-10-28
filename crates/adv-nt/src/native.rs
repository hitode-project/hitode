//! Native bodies of distribution functions.
//!
//! A function of a distribution image whose bytes its code identity table
//! confirms ([`crate::identity`]) can have a host body. Translated calls to
//! the function branch to a thunk instead ([`adv_machine::Sys::redirects`]),
//! whose host call runs the body without leaving translated code. The
//! interpreter runs Wine's own code, and so does a call the body declines:
//! it resumes at a trampoline preserving the first instruction's effect
//! before continuing at the second. A body therefore has the function's effect
//! whenever it accepts a call, and declines anything else, such as memory
//! the function would fault on.

use std::collections::HashMap;

use adv_cpu::backend::{Guest, Prot};
use adv_machine::layout::PAGE;
use adv_machine::{Conv, Env, Ext, HostFn, Outcome, Sys};
use iced_x86::{Decoder, DecoderOptions, FlowControl, OpKind};

use crate::identity::{self, Images};

mod crt;
mod dib;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
mod input;
mod linked;
mod mixer;
mod nls;
mod pixels;
mod png;
mod zlib;
pub use png::{PngFilter, PngRows};
pub use zlib::InflateFast;

/// The module the bodies' thunks are registered under.
pub const MODULE: &str = "hitode.native";

/// A host body: the function's return value, or None to decline before
/// any observable effect.
type BodyFn = fn(&mut Env<'_>) -> Option<u64>;

struct Body {
    /// The image and function, which also names the thunk.
    name: std::borrow::Cow<'static, str>,
    conv: Conv,
    body: BodyFn,
    /// Runs the function itself after the body declined.
    handler: adv_machine::Handler,
}

/// Calls whose body declined, so that Wine's own code ran.
#[derive(Default)]
struct Declines(u64);

/// The calls whose body declined so far.
pub fn declines(ext: &Ext) -> u64 {
    ext.try_get::<Declines>().map_or(0, |declines| declines.0)
}

static BODIES: std::sync::LazyLock<Vec<Body>> = std::sync::LazyLock::new(|| {
    let mut bodies = vec![
        Body {
            name: "ntdll.dll!memcpy".into(),
            conv: Conv::Cdecl { argc: 3 },
            body: memmove,
            handler: resume_original,
        },
        Body {
            name: "dsound.dll!DSOUND_MixToPrimary".into(),
            conv: Conv::Cdecl { argc: 6 },
            body: mixer::mix,
            handler: mixer::resume,
        },
        Body {
            name: "ntdll.dll!NtQueryPerformanceCounter".into(),
            conv: Conv::Stdcall { argc: 2 },
            body: query_performance_counter,
            handler: resume_original,
        },
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        Body {
            name: "win32u.dll!NtUserGetKeyState".into(),
            conv: Conv::Stdcall { argc: 1 },
            body: input::key_state,
            handler: resume_original,
        },
        Body {
            name: "kernelbase.dll!MultiByteToWideChar".into(),
            conv: Conv::Stdcall { argc: 6 },
            body: nls::multi_byte_to_wide_char,
            handler: resume_original,
        },
        Body {
            name: "kernelbase.dll!WideCharToMultiByte".into(),
            conv: Conv::Stdcall { argc: 8 },
            body: nls::wide_char_to_multi_byte,
            handler: resume_original,
        },
        Body {
            name: "ntdll.dll!RtlMultiByteToUnicodeN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: nls::rtl_multi_byte_to_unicode_n,
            handler: resume_original,
        },
        Body {
            name: "ntdll.dll!RtlUnicodeToMultiByteN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: nls::rtl_unicode_to_multi_byte_n,
            handler: resume_original,
        },
        Body {
            name: "ntdll.dll!RtlMultiByteToUnicodeSize".into(),
            conv: Conv::Stdcall { argc: 3 },
            body: nls::rtl_multi_byte_to_unicode_size,
            handler: resume_original,
        },
        Body {
            name: "ntdll.dll!RtlUnicodeToMultiByteSize".into(),
            conv: Conv::Stdcall { argc: 3 },
            body: nls::rtl_unicode_to_multi_byte_size,
            handler: resume_original,
        },
        Body {
            name: "ntdll.dll!RtlOemToUnicodeN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: nls::rtl_oem_to_unicode_n,
            handler: resume_original,
        },
        Body {
            name: "ntdll.dll!RtlUnicodeToOemN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: nls::rtl_unicode_to_oem_n,
            handler: resume_original,
        },
    ];
    bodies.extend(crt::bodies());
    for version in 24..=43 {
        bodies.push(Body {
            name: format!("d3dx9_{version}.dll!d3dx_load_pixels_from_pixels").into(),
            conv: Conv::Cdecl { argc: 6 },
            body: pixels::load,
            handler: resume_original,
        });
    }
    bodies.extend(png::bodies());
    bodies.extend(zlib::bodies());
    bodies.extend(linked::bodies());
    bodies
});

/// The host functions of the bodies. A linked helper's thunk names its
/// body and address, as `body@address`.
pub(crate) fn host_fn(name: &str) -> Option<HostFn> {
    let name = name.split_once('@').map_or(name, |(body, _)| body);
    let (index, body) = bodies().enumerate().find(|(_, body)| body.name == name)?;
    Some(HostFn {
        leaf: None,
        conv: body.conv,
        handler: run,
        data: index as u64,
        trace: false,
        guest: None,
    })
}

fn bodies() -> impl Iterator<Item = &'static Body> {
    BODIES.iter().chain(dib::BODIES)
}

/// The handler of a body's thunk: the body, or the function itself when
/// the body declines.
fn run(env: &mut Env<'_>) -> Outcome {
    let index = env.sys.thunks.get(env.thunk).f.data as usize;
    let body = BODIES
        .get(index)
        .unwrap_or_else(|| &dib::BODIES[index - BODIES.len()]);
    match (body.body)(env) {
        Some(value) => Outcome::Return64(value),
        None => {
            env.ext.get_or_default::<Declines>().0 += 1;
            (body.handler)(env)
        }
    }
}

/// The trampolines of this process: their page, its used bytes, and each
/// bound function's trampoline by thunk index.
#[derive(Default)]
struct Trampolines {
    page: u32,
    used: u32,
    by_thunk: HashMap<u32, u32>,
}

/// Binds the bodies of the image `pe`, mapped at `base`.
pub(crate) fn bind(cpu: &mut dyn Guest, sys: &mut Sys, ext: &mut Ext, base: u32, pe: &adv_pe::Pe) {
    if !ext
        .try_get::<Images>()
        .is_some_and(|images| images.0.contains_key(&base))
    {
        // A game image: only helpers it links statically are known.
        for section in &pe.sections {
            if section.characteristics & adv_pe::scn::MEM_EXECUTE != 0 {
                let start = base + section.virtual_address;
                linked::bind(cpu, sys, ext, start, section.mem_size());
            }
        }
        return;
    }
    let mut relocations = pe.relocations().unwrap_or_default();
    relocations.sort_unstable();
    bind_relocated(cpu, sys, ext, base, &relocations);
    pixels::prepare(cpu, ext, base, pe, &relocations);
}

pub(crate) fn bind_relocated(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    ext: &mut Ext,
    base: u32,
    relocations: &[u32],
) {
    let Some(module) = ext
        .try_get::<Images>()
        .and_then(|images| images.0.get(&base))
        .cloned()
    else {
        return;
    };
    if !BODIES
        .iter()
        .chain(dib::BODIES)
        .any(|body| body.name.starts_with(module.name.as_str()))
    {
        return;
    }
    let Some(table) = module.table() else {
        return;
    };
    for body in BODIES.iter().chain(dib::BODIES) {
        let Some(name) = body
            .name
            .strip_prefix(module.name.as_str())
            .and_then(|name| name.strip_prefix('!'))
        else {
            continue;
        };
        let Some(function) = table.named(name) else {
            continue;
        };
        let entry = base + function.rva;
        let relocated = |address: u32| {
            relocations
                .binary_search(&address.wrapping_sub(base))
                .is_ok()
        };
        if !identity::intact(cpu, entry, function, relocated) {
            log::debug!("{}: bytes differ from the distribution", body.name);
            continue;
        }
        if let Err(error) = bind_one(cpu, sys, ext, &body.name, entry, function.size) {
            log::warn!("{}: no native body: {error}", body.name);
        }
    }
}

fn bind_one(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    ext: &mut Ext,
    name: &str,
    entry: u32,
    size: u32,
) -> Result<(), String> {
    let mut original = vec![0; size as usize];
    cpu.read(entry, &mut original).map_err(|e| e.to_string())?;
    let first = Decoder::with_ip(32, &original, u64::from(entry), DecoderOptions::NONE).decode();
    let call =
        first.flow_control() == FlowControl::Call && first.op0_kind() == OpKind::NearBranch32;
    if first.is_invalid() || (!call && first.flow_control() != FlowControl::Next) {
        return Err("the first instruction cannot move".into());
    }
    let length = first.len();
    // A PIC prologue's call observes its original return address. Pushing
    // that address and jumping preserves both the helper and its return.
    let mut trampoline = if call {
        let mut code = vec![0x68];
        code.extend_from_slice(&(entry + length as u32).to_le_bytes());
        code
    } else {
        original[..length].to_vec()
    };
    trampoline.push(0xe9);
    let resume = if call {
        first.near_branch32()
    } else {
        entry + length as u32
    };
    let thunk = sys
        .thunks
        .publish(cpu, MODULE, name)
        .map_err(|e| e.to_string())?;
    let index = sys.thunks.published_index(MODULE, name).ok_or("no thunk")?;
    let trampolines = ext.get_or_default::<Trampolines>();
    if trampolines.page == 0 || trampolines.used + trampoline.len() as u32 + 4 > PAGE {
        trampolines.page = sys
            .vm
            .alloc(cpu, PAGE, Prot::RWX)
            .map_err(|e| e.to_string())?;
        trampolines.used = 0;
    }
    let at = trampolines.page + trampolines.used;
    let next = at + trampoline.len() as u32 + 4;
    trampoline.extend_from_slice(&resume.wrapping_sub(next).to_le_bytes());
    cpu.write(at, &trampoline).map_err(|e| e.to_string())?;
    trampolines.used = next - trampolines.page;
    trampolines.by_thunk.insert(index, at);
    let target = if name == "dsound.dll!DSOUND_MixToPrimary" {
        mixer::entry(cpu, sys, thunk)?
    } else if name.starts_with("win32u.so!halftone_") {
        dib::entry(cpu, sys, thunk)?
    } else {
        thunk
    };
    sys.redirects.insert(entry, (target, original.into()));
    Ok(())
}

/// The ordinary handler of a body's thunk, entered when the body declined:
/// the function runs from its trampoline with the caller's frame.
fn resume_original(env: &mut Env<'_>) -> Outcome {
    let Some(&trampoline) = env
        .ext
        .get_or_default::<Trampolines>()
        .by_thunk
        .get(&env.thunk)
    else {
        return Outcome::Fail("native body without a trampoline".into());
    };
    match env.checkpoint() {
        Ok(mut checkpoint) => {
            checkpoint.state.eip = trampoline;
            Outcome::ResumeExecution(checkpoint)
        }
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

/// memmove(dst, src, n): EAX and EDX are `dst`, as Wine's code leaves them.
fn memmove(env: &mut Env<'_>) -> Option<u64> {
    let (dst, src, n) = (env.arg(0), env.arg(1), env.arg(2));
    if n != 0 {
        env.cpu.copy_memory(src, dst, n as usize).ok()?;
    }
    Some(u64::from(dst) << 32 | u64::from(dst))
}

// spec: NAT-BODY-2
fn query_performance_counter(env: &mut Env<'_>) -> Option<u64> {
    let (counter, frequency) = (env.arg(0), env.arg(1));
    if counter == 0
        || !env.cpu.accessible(counter, 8, Prot::WRITE)
        || (frequency != 0 && !env.cpu.accessible(frequency, 8, Prot::WRITE))
    {
        return None;
    }
    // The syscall boundary's system-time reads advance the virtual clock.
    if env.sys.clock.is_virtual() {
        crate::kuser::refresh(env);
        crate::server::set_time(env);
    }
    let ticks = crate::system::performance_counter(env);
    env.cpu.write(counter, &ticks.to_le_bytes()).ok()?;
    if frequency != 0 {
        env.cpu
            .write(frequency, &crate::system::TICKS_PER_SECOND.to_le_bytes())
            .ok()?;
    }
    env.sys.poll_observation();
    Some(u64::from(crate::status::SUCCESS))
}
