//! Native bodies of distribution functions.
//!
//! A function of a distribution image whose bytes its code identity table
//! confirms ([`crate::identity`]) can have a host body. Translated calls to
//! the function branch to a thunk instead ([`adv_machine::Sys::redirects`]),
//! whose host call runs the body without leaving translated code. The
//! interpreter runs ordinary bodies as Wine code, and so does a declined call:
//! it resumes at a trampoline preserving the first instruction's effect
//! before continuing at the second. A body therefore has the function's effect
//! whenever it accepts a call, and declines anything else, such as memory
//! the function would fault on. Required text services supply a complete handler
//! and replace the entry in both execution modes, without a trampoline.
//!
//! Direct bodies share their memory implementation between generated and full
//! paths. Service bodies reach shared clock and file storage through BodyContext,
//! without borrowing Sys or Ext. Diagnostics record host memory work; debugger
//! sessions also keep durations and outliers. Immutable file inputs retain derived
//! values by pinned identity, and temporary snapshots reuse execution-owner scratch.

use std::collections::HashMap;

use adv_cpu::backend::{Guest, HostAccess, Prot};
use adv_machine::layout::PAGE;
use adv_machine::{Conv, Env, Ext, HostFn, Outcome, Sys};
use iced_x86::{Decoder, DecoderOptions, FlowControl, OpKind};

use crate::identity::{self, Images};

mod crt;
mod dib;
mod fonts;
pub use fonts::FontFrontend;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
mod input;
mod linked;
mod mixer;
mod nls;
mod own_calls;
mod pixels;
mod png;
mod services;
mod zlib;
pub use png::{PngFilter, PngRows};

/// The module the bodies' thunks are registered under.
pub const MODULE: &str = "hitode.native";

/// A host body: the function's return value, or None to decline before
/// any observable effect.
type BodyFn = fn(&mut Env<'_>) -> Option<u64>;

struct Body {
    /// The image and function, which also names the thunk.
    name: std::borrow::Cow<'static, str>,
    conv: Conv,
    body: Option<BodyFn>,
    /// Runs the function itself after the body declined.
    handler: adv_machine::Handler,
    /// The body as generated code calls it directly (A64-CALL-2); `body`
    /// is then unused.
    direct: Option<adv_cpu::body::Body>,
}

/// Calls whose body declined, so that Wine's own code ran.
#[derive(Default)]
struct Declines(u64);

/// The calls whose body declined so far.
pub fn declines(ext: &Ext) -> u64 {
    ext.try_get::<Declines>().map_or(0, |declines| declines.0)
}

thread_local! {
    static DEBUGGER_ATTACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The shell's existing debugger lifetime, on the guest execution thread.
pub fn debugger_attached(attached: bool) {
    DEBUGGER_ATTACHED.set(attached);
    adv_machine::debugger_attached(attached);
}

#[derive(Clone, Copy, Default)]
pub struct BodyCount {
    pub name: &'static str,
    pub calls: u64,
    pub declines: u64,
    pub timed_calls: u64,
    pub nanos: u64,
    pub access: HostAccess,
}
impl BodyCount {
    pub fn since(self, base: Self) -> Self {
        Self {
            name: self.name,
            calls: self.calls - base.calls,
            declines: self.declines - base.declines,
            timed_calls: self.timed_calls - base.timed_calls,
            nanos: self.nanos - base.nanos,
            access: access_delta(self.access, base.access),
        }
    }
}

#[derive(Clone, Copy)]
pub struct TailCall {
    pub body: usize,
    pub call: u64,
    pub name: &'static str,
    pub arguments: [u32; 6],
    pub duration: std::time::Duration,
    pub access: HostAccess,
}

pub struct Boundary {
    pub bodies: Vec<BodyCount>,
    tails: [Option<TailCall>; 16],
    next: usize,
}
impl Default for Boundary {
    fn default() -> Self {
        Self {
            bodies: bodies()
                .map(|body| BodyCount {
                    name: &body.name,
                    ..BodyCount::default()
                })
                .collect(),
            tails: [None; 16],
            next: 0,
        }
    }
}
impl Boundary {
    pub fn tails(&self) -> impl Iterator<Item = &TailCall> {
        (0..16).filter_map(|i| self.tails[(self.next + i) % 16].as_ref())
    }
    fn record(
        &mut self,
        index: usize,
        declined: bool,
        access: HostAccess,
        elapsed: Option<std::time::Duration>,
        arguments: [u32; 6],
    ) {
        let row = &mut self.bodies[index];
        row.calls += 1;
        row.declines += u64::from(declined);
        row.access.reads += access.reads;
        row.access.read_bytes += access.read_bytes;
        row.access.writes += access.writes;
        row.access.written_bytes += access.written_bytes;
        row.access.shared_copies += access.shared_copies;
        row.access.fallbacks += access.fallbacks;
        row.access.tlb_misses += access.tlb_misses;
        if let Some(duration) = elapsed {
            let nanos = duration.as_nanos() as u64;
            if row.timed_calls != 0 && nanos > 100_000 && nanos > 16 * (row.nanos / row.timed_calls)
            {
                self.tails[self.next] = Some(TailCall {
                    body: index,
                    call: row.calls,
                    name: row.name,
                    arguments,
                    duration,
                    access,
                });
                self.next = (self.next + 1) % 16;
            }
            row.timed_calls += 1;
            row.nanos += nanos;
        }
    }
}
fn access_delta(after: HostAccess, before: HostAccess) -> HostAccess {
    HostAccess {
        reads: after.reads - before.reads,
        read_bytes: after.read_bytes - before.read_bytes,
        writes: after.writes - before.writes,
        written_bytes: after.written_bytes - before.written_bytes,
        shared_copies: after.shared_copies - before.shared_copies,
        fallbacks: after.fallbacks - before.fallbacks,
        tlb_misses: after.tlb_misses - before.tlb_misses,
    }
}

#[derive(Default)]
struct Scratch(Vec<u8>);

/// Reusable temporary bytes, valid until the next scratch request on this Ext.
/// They hold stale data from earlier requests: callers write before reading.
pub fn scratch(ext: &mut Ext, len: usize) -> &mut [u8] {
    let bytes = &mut ext.get_or_default::<Scratch>().0;
    bytes.resize(len, 0);
    bytes
}

struct MemoValue {
    pinned: adv_cpu::backend::Pinned,
    value: Box<dyn std::any::Any>,
}
#[derive(Default)]
struct Memos(HashMap<(u32, usize, std::any::TypeId), MemoValue>);
impl Memos {
    fn get<T: 'static>(
        &mut self,
        cpu: &dyn Guest,
        addr: u32,
        len: usize,
        derive: impl FnOnce(&[u8]) -> T,
    ) -> Option<&T> {
        let key = (addr, len, std::any::TypeId::of::<T>());
        if !self
            .0
            .get(&key)
            .is_some_and(|memo| cpu.still_pinned(&memo.pinned))
        {
            let pinned = cpu.pinned(addr, len)?;
            let mut bytes = vec![0; len];
            cpu.read(addr, &mut bytes).ok()?;
            self.0.insert(
                key,
                MemoValue {
                    pinned,
                    value: Box::new(derive(&bytes)),
                },
            );
        }
        self.0.get(&key)?.value.downcast_ref()
    }
}

/// Derive immutable guest data once per retained file identity.
pub fn memo<'a, T: 'static>(
    env: &'a mut Env<'_>,
    addr: u32,
    len: usize,
    derive: impl FnOnce(&[u8]) -> T,
) -> Option<&'a T> {
    env.ext
        .get_or_default::<Memos>()
        .get(env.cpu, addr, len, derive)
}

static BODIES: std::sync::LazyLock<Vec<Body>> = std::sync::LazyLock::new(|| {
    let mut bodies = vec![
        Body {
            name: "ntdll.dll!memcpy".into(),
            conv: Conv::Cdecl { argc: 3 },
            body: None,
            handler: resume_original,
            direct: Some(crt::MEMMOVE),
        },
        Body {
            name: "dsound.dll!DSOUND_MixToPrimary".into(),
            conv: Conv::Cdecl { argc: 6 },
            body: Some(mixer::mix),
            handler: mixer::resume,
            direct: None,
        },
        Body {
            name: "ntdll.dll!NtQueryPerformanceCounter".into(),
            conv: Conv::Stdcall { argc: 2 },
            body: None,
            handler: resume_original,
            direct: Some(services::NT_COUNTER),
        },
        Body {
            name: "ntdll.dll!RtlQueryPerformanceCounter".into(),
            conv: Conv::Stdcall { argc: 1 },
            body: None,
            handler: resume_original,
            direct: Some(services::COUNTER),
        },
        Body {
            name: "kernelbase.dll!QueryPerformanceCounter".into(),
            conv: Conv::Stdcall { argc: 1 },
            body: None,
            handler: resume_original,
            direct: Some(services::COUNTER),
        },
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        Body {
            name: "win32u.dll!NtUserGetKeyState".into(),
            conv: Conv::Stdcall { argc: 1 },
            body: None,
            handler: resume_original,
            direct: Some(input::KEY_STATE),
        },
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        Body {
            name: "win32u.dll!NtUserGetKeyboardState".into(),
            conv: Conv::Stdcall { argc: 1 },
            body: None,
            handler: resume_original,
            direct: Some(input::KEYBOARD_STATE),
        },
        Body {
            name: "kernelbase.dll!MultiByteToWideChar".into(),
            conv: Conv::Stdcall { argc: 6 },
            body: Some(nls::multi_byte_to_wide_char),
            handler: resume_original,
            direct: None,
        },
        Body {
            name: "kernelbase.dll!WideCharToMultiByte".into(),
            conv: Conv::Stdcall { argc: 8 },
            body: Some(nls::wide_char_to_multi_byte),
            handler: resume_original,
            direct: None,
        },
        Body {
            name: "ntdll.dll!RtlMultiByteToUnicodeN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: Some(nls::rtl_multi_byte_to_unicode_n),
            handler: resume_original,
            direct: None,
        },
        Body {
            name: "ntdll.dll!RtlUnicodeToMultiByteN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: Some(nls::rtl_unicode_to_multi_byte_n),
            handler: resume_original,
            direct: None,
        },
        Body {
            name: "ntdll.dll!RtlMultiByteToUnicodeSize".into(),
            conv: Conv::Stdcall { argc: 3 },
            body: Some(nls::rtl_multi_byte_to_unicode_size),
            handler: resume_original,
            direct: None,
        },
        Body {
            name: "ntdll.dll!RtlUnicodeToMultiByteSize".into(),
            conv: Conv::Stdcall { argc: 3 },
            body: Some(nls::rtl_unicode_to_multi_byte_size),
            handler: resume_original,
            direct: None,
        },
        Body {
            name: "ntdll.dll!RtlOemToUnicodeN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: Some(nls::rtl_oem_to_unicode_n),
            handler: resume_original,
            direct: None,
        },
        Body {
            name: "ntdll.dll!RtlUnicodeToOemN".into(),
            conv: Conv::Stdcall { argc: 5 },
            body: Some(nls::rtl_unicode_to_oem_n),
            handler: resume_original,
            direct: None,
        },
    ];
    bodies.extend(crt::bodies());
    bodies.extend(services::bodies());
    for version in 24..=43 {
        bodies.push(Body {
            name: format!("d3dx9_{version}.dll!d3dx_load_pixels_from_pixels").into(),
            conv: Conv::Cdecl { argc: 6 },
            body: Some(pixels::load),
            handler: resume_original,
            direct: None,
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
    BODIES.iter().chain(dib::BODIES).chain(fonts::BODIES)
}

fn body_at(index: usize) -> &'static Body {
    if let Some(body) = BODIES.get(index) {
        body
    } else {
        let index = index - BODIES.len();
        dib::BODIES
            .get(index)
            .unwrap_or_else(|| &fonts::BODIES[index - dib::BODIES.len()])
    }
}

/// The handler of a body's thunk: the body, or the function itself when
/// the body declines.
fn run(env: &mut Env<'_>) -> Outcome {
    let index = env.sys.thunks.get(env.thunk).f.data as usize;
    let body = body_at(index);
    run_body(env, index, body)
}

fn run_body(env: &mut Env<'_>, index: usize, body: &'static Body) -> Outcome {
    let diagnostics = env.sys.diagnostics() != 0;
    let timed = DEBUGGER_ATTACHED.get();
    let arguments = if timed {
        std::array::from_fn(|i| {
            if i < usize::from(body.conv.argc()) {
                env.arg(i as u32)
            } else {
                0
            }
        })
    } else {
        [0; 6]
    };
    let before = diagnostics.then(|| env.cpu.host_access());
    let start = timed.then(std::time::Instant::now);
    let declined_before = diagnostics.then(|| declines(env.ext));
    let result = if let Some(direct) = body.direct {
        Some(run_full(env, direct, body.handler))
    } else {
        match body.body {
            Some(run) => run(env).map(Outcome::Return64),
            None => Some((body.handler)(env)),
        }
    };
    let declined =
        result.is_none() || declined_before.is_some_and(|before| declines(env.ext) != before);
    let elapsed = start.map(|start| start.elapsed());
    let access = before.map(|before| access_delta(env.cpu.host_access(), before));
    #[cfg(any(debug_assertions, test))]
    assert!(
        result.is_some() || access.is_none_or(|access| access.writes == 0),
        "{} declined after guest writes (NAT-BODY-1)",
        body.name
    );
    if let Some(access) = access {
        env.ext
            .get_or_default::<Boundary>()
            .record(index, declined, access, elapsed, arguments);
    }
    match result {
        Some(outcome) => outcome,
        None => {
            env.ext.get_or_default::<Declines>().0 += 1;
            (body.handler)(env)
        }
    }
}

/// What the full path of direct bodies reuses.
#[derive(Default)]
struct FullScratch(adv_cpu::body::Scratch);

/// Each bound direct body's entry and inputs, reached by thunk index.
#[derive(Default)]
struct Entries(Vec<Option<(u32, adv_cpu::body::DirectBody)>>);
impl Entries {
    fn insert(&mut self, thunk: u32, entry: u32, direct: adv_cpu::body::DirectBody) {
        if self.0.len() <= thunk as usize {
            self.0.resize(thunk as usize + 1, None);
        }
        self.0[thunk as usize] = Some((entry, direct));
    }
}

/// A direct body on the full path, which completes every access a guest
/// instruction would and raises the guest's fault where it would fault.
fn run_full(
    env: &mut Env<'_>,
    body: adv_cpu::body::Body,
    handler: adv_machine::Handler,
) -> Outcome {
    use adv_cpu::backend::{Fault, FaultKind, Reg};
    use adv_cpu::body::{ECX, EDX, GuestMem, Input, Stop};
    let bound = env
        .ext
        .get_or_default::<Entries>()
        .0
        .get(env.thunk as usize)
        .copied()
        .flatten();
    let direct = bound.map_or(body.direct, |(_, direct)| direct);
    let inputs = std::array::from_fn(|n| match direct.inputs[n] {
        Some(Input::Stack(k)) => env.arg(u32::from(k)),
        Some(Input::Const(value)) => value,
        Some(Input::Eax) => env.cpu.reg(Reg::Eax),
        Some(Input::Ecx) => env.cpu.reg(Reg::Ecx),
        Some(Input::Edx) => env.cpu.reg(Reg::Edx),
        None => 0,
    });
    let mut scratch = std::mem::take(&mut env.ext.get_or_default::<FullScratch>().0);
    let services = env.sys.services(env.ext);
    let result = (body.full)(
        &mut GuestMem::for_body(env.cpu, &mut scratch, services),
        inputs,
    );
    env.ext.get_or_default::<FullScratch>().0 = scratch;
    let outputs = body.direct.outputs;
    match result {
        Ok(ret) => {
            if outputs & ECX != 0 {
                env.cpu.set_reg(Reg::Ecx, ret.ecx);
            }
            if outputs & EDX != 0 {
                Outcome::Return64(ret.value)
            } else {
                Outcome::Return(ret.value as u32)
            }
        }
        Err(stop @ (Stop::Fault { .. } | Stop::DivideByZero)) => {
            let Some((pc, _)) = bound else {
                return Outcome::Fail("direct body without an entry".into());
            };
            let kind = match stop {
                Stop::Fault {
                    address,
                    write: true,
                } => FaultKind::Write(address),
                Stop::Fault {
                    address,
                    write: false,
                } => FaultKind::Read(address),
                Stop::DivideByZero => FaultKind::IntegerDivideByZero,
                _ => unreachable!(),
            };
            Outcome::Raise(Fault { kind, pc })
        }
        Err(Stop::Guest) => {
            env.ext.get_or_default::<Declines>().0 += 1;
            (handler)(env)
        }
        Err(Stop::Slow) => Outcome::Fail("the full path left a direct body".into()),
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
    sys.body_services = Some(|sys, ext, services| {
        if let Some(link) = ext.try_get::<crate::server::Link>() {
            services.thread = link.known_tid(sys.current).unwrap_or(0);
            services.state = std::rc::Rc::as_ptr(&link.files).cast();
        } else {
            services.thread = 0;
        }
    });
    let Some(module) = ext
        .try_get::<Images>()
        .and_then(|images| images.0.get(&base))
        .cloned()
    else {
        return;
    };
    let bodies = BODIES
        .iter()
        .chain(dib::BODIES)
        .chain(fonts::BODIES)
        .any(|body| body.name.starts_with(module.name.as_str()));
    let own = own_calls::covers(&module.name);
    if !bodies && !own {
        return;
    }
    let Some(table) = module.table() else {
        return;
    };
    let relocated = |address: u32| {
        relocations
            .binary_search(&address.wrapping_sub(base))
            .is_ok()
    };
    if own {
        own_calls::bind(cpu, sys, ext, base, &module.name, &table, relocated);
    }
    if !bodies {
        return;
    }
    // Text bodies need the runtime's frontend; machines assembled without
    // text services keep Wine's code.
    let text = ext.try_get::<FontFrontend>().is_some();
    let bodies = BODIES.iter().chain(dib::BODIES);
    for body in bodies.chain(fonts::BODIES.iter().filter(|_| text)) {
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
    if fonts::BODIES.iter().any(|body| body.name == name) {
        let thunk = sys
            .thunks
            .publish(cpu, MODULE, name)
            .map_err(|e| e.to_string())?;
        sys.redirects.insert(entry, (thunk, original.into()));
        sys.required_redirects.insert(entry);
        return Ok(());
    }
    let first = Decoder::with_ip(32, &original, u64::from(entry), DecoderOptions::NONE).decode();
    let call =
        first.flow_control() == FlowControl::Call && first.op0_kind() == OpKind::NearBranch32;
    if first.is_invalid() || (!call && first.flow_control() != FlowControl::Next) {
        return Err("the first instruction cannot move".into());
    }
    let length = first.len();
    // A PIC prologue's call observes its original return address. Pushing
    // that address and jumping preserves both the helper and its return.
    let moved = if call {
        let mut code = vec![0x68];
        code.extend_from_slice(&(entry + length as u32).to_le_bytes());
        code
    } else {
        original[..length].to_vec()
    };
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
    let at = trampoline(cpu, sys, ext, moved, resume)?;
    ext.get_or_default::<Trampolines>()
        .by_thunk
        .insert(index, at);
    let target = if name == "dsound.dll!DSOUND_MixToPrimary" {
        mixer::entry(cpu, sys, thunk)?
    } else if name.starts_with("win32u.so!halftone_") {
        dib::entry(cpu, sys, thunk)?
    } else {
        thunk
    };
    let body_name = name.split_once('@').map_or(name, |(body, _)| body);
    if let Some(direct) = bodies()
        .find(|body| body.name == body_name)
        .and_then(|body| body.direct)
    {
        let mut direct = direct.direct;
        if name.ends_with("!inflate_fast") {
            direct.inputs = zlib::inputs(cpu, ext, entry)?;
        }
        sys.direct_bodies.insert(entry, direct);
        ext.get_or_default::<Entries>().insert(index, entry, direct);
    }
    sys.redirects.insert(entry, (target, original.into()));
    Ok(())
}

/// Places `code` followed by a jump to `resume` in the process's trampoline
/// pages, and returns its address.
fn trampoline(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    ext: &mut Ext,
    mut code: Vec<u8>,
    resume: u32,
) -> Result<u32, String> {
    code.push(0xe9);
    let trampolines = ext.get_or_default::<Trampolines>();
    if trampolines.page == 0 || trampolines.used + code.len() as u32 + 4 > PAGE {
        trampolines.page = sys
            .vm
            .alloc(cpu, PAGE, Prot::RWX)
            .map_err(|e| e.to_string())?;
        trampolines.used = 0;
    }
    let at = trampolines.page + trampolines.used;
    let next = at + code.len() as u32 + 4;
    code.extend_from_slice(&resume.wrapping_sub(next).to_le_bytes());
    cpu.write(at, &code).map_err(|e| e.to_string())?;
    trampolines.used = next - trampolines.page;
    Ok(at)
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

#[cfg(test)]
mod tests;
