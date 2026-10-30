//! Wine 11's regparm(3) mixer: EAX=device, EDX=output, ECX=frames,
//! with all_stopped on the stack. Samples and scratch stay in guest storage.

use adv_cpu::backend::{Fault, FaultKind, Guest, GuestExt, Prot, Reg};
use adv_machine::{Env, Ext, Outcome};

#[path = "mixer_float.rs"]
mod arithmetic;
#[path = "mixer_layout.rs"]
mod layout;
use arithmetic::Float;
use layout::{buffer as b, device as d, volume as v, *};

const GET: [&str; 6] = ["get8", "get16", "get24", "get32", "getieee32", "get_mono"];
const PUT: [&str; 10] = [
    "putieee32",
    "putieee32_sum",
    "put_mono2stereo",
    "put_mono2quad",
    "put_mono2surround51",
    "put_stereo2quad",
    "put_stereo2surround51",
    "put_quad2stereo",
    "put_surround512stereo",
    "put_surround712stereo",
];

#[derive(Clone, Copy)]
struct Prepared {
    entry: u32,
    fir: u32,
    get: [u32; 6],
    put: [u32; 10],
    malloc: u32,
    realloc: u32,
    nt: Nt,
}

pub(super) fn prepare(
    cpu: &dyn Guest,
    sys: &mut adv_machine::Sys,
    ext: &mut Ext,
    entry: u32,
) -> Result<(), String> {
    let module = ext
        .get::<crate::identity::Images>()
        .containing(entry)
        .ok_or("missing mixer image")?;
    let table = module.table().ok_or("missing mixer identity")?;
    let address = |name: &str| {
        table
            .named(name)
            .map(|f| module.base + f.rva)
            .ok_or_else(|| format!("missing mixer helper {name}"))
    };
    let mut get = [0; 6];
    let mut put = [0; 10];
    for (at, name) in get.iter_mut().zip(GET) {
        *at = address(name)?;
    }
    for (at, name) in put.iter_mut().zip(PUT) {
        *at = address(name)?;
    }
    let mut headers = [0; 4096];
    cpu.read(module.base, &mut headers)
        .map_err(|e| e.to_string())?;
    let nt = u32::from_le_bytes(headers[0x3c..0x40].try_into().unwrap()) as usize;
    let imports = u32::from_le_bytes(headers[nt + 128..nt + 132].try_into().unwrap());
    let mut descriptor = module.base + imports;
    let (mut malloc, mut realloc) = (0, 0);
    loop {
        let names = cpu.read_u32(descriptor).map_err(|e| e.to_string())?;
        let dll = cpu.read_u32(descriptor + 12).map_err(|e| e.to_string())?;
        if dll == 0 {
            break;
        }
        let iat = cpu.read_u32(descriptor + 16).map_err(|e| e.to_string())?;
        let mut i = 0;
        loop {
            let symbol = cpu
                .read_u32(module.base + names + i * 4)
                .map_err(|e| e.to_string())?;
            if symbol == 0 {
                break;
            }
            if symbol & 0x8000_0000 == 0 {
                let mut name = [0; 8];
                cpu.read(module.base + symbol + 2, &mut name)
                    .map_err(|e| e.to_string())?;
                if name.starts_with(b"malloc\0") {
                    malloc = module.base + iat + i * 4;
                }
                if name.starts_with(b"realloc\0") {
                    realloc = module.base + iat + i * 4;
                }
            }
            i += 1;
        }
        descriptor += 20;
    }
    let ntdll = ext
        .get::<crate::identity::Images>()
        .0
        .values()
        .find(|m| m.name == "ntdll.dll")
        .ok_or("missing ntdll image")?;
    let relocations = relocations(cpu, module.base).ok_or("missing mixer relocations")?;
    let mut checks = Vec::new();
    for name in GET.into_iter().chain(PUT).chain([
        "secondarybuffer_is_audible",
        "mixieee32",
        "__divdi3",
        "__divmoddi4",
        "DSOUND_CheckEvent.part.0",
    ]) {
        let function = table.named(name).ok_or("missing mixer dependency")?;
        let at = module.base + function.rva;
        if !crate::identity::intact(cpu, at, function, |at| {
            relocations.binary_search(&at).is_ok()
        }) {
            return Err(format!("modified mixer helper {name}"));
        }
        let mut bytes = vec![0; function.size as usize];
        cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
        checks.push((at, bytes.into_boxed_slice()));
    }
    let nt = nt_inputs(cpu, ntdll, &mut checks)?;
    sys.redirect_checks.insert(entry, checks);
    let prepared = Prepared {
        nt,
        entry,
        fir: module.base + FIR_RVA,
        get,
        put,
        malloc,
        realloc,
    };
    ext.insert(prepared);
    Ok(())
}

/// The sorted addresses of the image's HIGHLOW base relocations.
fn relocations(cpu: &dyn Guest, base: u32) -> Option<Vec<u32>> {
    let nt = cpu.read_u32(base + 0x3c).ok()?.checked_add(base)?;
    let reloc_at = cpu.read_u32(nt.checked_add(160)?).ok()?.checked_add(base)?;
    let reloc_len = cpu.read_u32(nt.checked_add(164)?).ok()?;
    if !cpu.accessible(reloc_at, reloc_len, Prot::READ) {
        return None;
    }
    let mut reloc = vec![0; reloc_len as usize];
    cpu.read(reloc_at, &mut reloc).ok()?;
    let mut relocations = Vec::new();
    let mut at = 0;
    while at < reloc.len() {
        let header = reloc.get(at..at + 8)?;
        let block = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        if block < 8 || !block.is_multiple_of(2) || at + block > reloc.len() {
            return None;
        }
        for offset in (at + 8..at + block).step_by(2) {
            let entry = u16::from_le_bytes(reloc[offset..offset + 2].try_into().unwrap());
            if entry >> 12 == 3 {
                relocations.push(
                    base + u32::from_le_bytes(reloc[at..at + 4].try_into().unwrap())
                        + u32::from(entry & 0xfff),
                );
            }
        }
        at += block;
    }
    relocations.sort_unstable();
    Some(relocations)
}

#[derive(Clone, Copy)]
struct Page {
    base: u32,
    pointer: *mut u8,
    write: bool,
}
const EMPTY: Page = Page {
    base: u32::MAX,
    pointer: std::ptr::null_mut(),
    write: false,
};

/// Ordinary pages are borrowed; shared, watched and file bytes use exact
/// guest loads/stores. No snapshot or temporary sample buffer is introduced.
struct Memory<'a> {
    cpu: &'a mut dyn Guest,
    pages: [Page; 16],
    next: usize,
}
impl<'a> Memory<'a> {
    fn new(cpu: &'a mut dyn Guest) -> Self {
        Self {
            cpu,
            pages: [EMPTY; 16],
            next: 0,
        }
    }
    fn page(&mut self, at: u32, write: bool) -> *mut u8 {
        let base = at & !4095;
        if let Some(page) = self
            .pages
            .iter()
            .find(|p| p.base == base && (!write || p.write))
        {
            return page.pointer;
        }
        let prot = if write { Prot::RW } else { Prot::READ };
        let pointer = if !self.cpu.accessible(base, 4096, prot) {
            std::ptr::null_mut()
        } else if write {
            self.cpu
                .writable_bytes(base, 4096)
                .map_or(std::ptr::null_mut(), |p| p.as_mut_ptr())
        } else {
            self.cpu
                .shared_bytes(base, 4096)
                .map_or(std::ptr::null_mut(), |p| p.as_ptr().cast_mut())
        };
        self.pages[self.next] = Page {
            base,
            pointer,
            write,
        };
        self.next = (self.next + 1) % self.pages.len();
        pointer
    }
    fn read<const N: usize>(&mut self, at: u32) -> Result<[u8; N], Error> {
        let mut bytes = [0; N];
        if (at & 4095) as usize + N <= 4096 {
            let page = self.page(at, false);
            if !page.is_null() {
                // SAFETY: this page is borrowed and the entire unit lies in it.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        page.add((at & 4095) as usize),
                        bytes.as_mut_ptr(),
                        N,
                    )
                };
                return Ok(bytes);
            }
        }
        if !self.cpu.accessible(at, N as u32, Prot::READ) {
            return Err(self.fault(at, N as u32, false));
        }
        self.cpu
            .read(at, &mut bytes)
            .map_err(|_| self.fault(at, N as u32, false))?;
        Ok(bytes)
    }
    fn write<const N: usize>(&mut self, at: u32, bytes: [u8; N]) -> Result<(), Error> {
        if (at & 4095) as usize + N <= 4096 {
            let page = self.page(at, true);
            if !page.is_null() {
                // SAFETY: the ordinary page is writable and contains the unit.
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), page.add((at & 4095) as usize), N)
                };
                return Ok(());
            }
        }
        if !self.cpu.accessible(at, N as u32, Prot::WRITE) {
            return Err(self.fault(at, N as u32, true));
        }
        self.cpu
            .write(at, &bytes)
            .map_err(|_| self.fault(at, N as u32, true))
    }
    fn fault(&self, at: u32, len: u32, write: bool) -> Error {
        let prot = if write { Prot::WRITE } else { Prot::READ };
        let mut address = at;
        let mut remaining = len;
        while remaining != 0 {
            let chunk = remaining.min(4096 - (address & 4095));
            if !self.cpu.accessible(address, chunk, prot) {
                break;
            }
            address = address.wrapping_add(chunk);
            remaining -= chunk;
        }
        Error::Fault(if write {
            FaultKind::Write(address)
        } else {
            FaultKind::Read(address)
        })
    }
    fn word(&mut self, at: u32) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.read(at)?))
    }
    fn short(&mut self, at: u32) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.read(at)?))
    }
    fn long(&mut self, at: u32) -> Result<i64, Error> {
        Ok(i64::from_le_bytes(self.read(at)?))
    }
    fn float(&mut self, at: u32) -> Result<f32, Error> {
        Ok(f32::from_bits(self.word(at)?))
    }
    fn set(&mut self, at: u32, value: u32) -> Result<(), Error> {
        self.write(at, value.to_le_bytes())
    }
    fn set_float(&mut self, at: u32, value: f32) -> Result<(), Error> {
        self.set(at, value.to_bits())
    }
    fn fill(&mut self, at: u32, len: u32) -> Result<(), Error> {
        for i in 0..len {
            self.write(at.wrapping_add(i), [0])?;
        }
        Ok(())
    }
}

#[derive(Debug)]
enum Error {
    Fault(FaultKind),
    Helper(u32),
}
#[derive(Clone, Copy)]
enum Phase {
    Buffer,
    Temp,
    Ready,
    Cache,
    Convert,
    Positions,
    Notify,
    Unlock,
}
#[derive(Clone, Copy)]
struct Mix {
    device: u32,
    output: u32,
    frames: u32,
    stopped: u32,
    index: i32,
    buffer: u32,
    oldpos: u32,
    advance: u32,
    acc: i64,
    cache: u32,
    required: u32,
    phase: Phase,
    fp: Float,
    notify: i32,
    notify_phase: u8,
    first: i32,
    left: i32,
}
struct Call {
    target: u32,
    args: [u32; 2],
    argc: usize,
    pointer: u32,
    length: Option<(u32, u32)>,
}
enum Step {
    Done,
    Call(Call),
    Notify(u32),
    Wake(u32),
    WakeGuest(u32),
}

fn field(base: u32, offset: usize) -> u32 {
    base.wrapping_add(offset as u32)
}
fn channels(mem: &mut Memory<'_>, device: u32) -> Result<u32, Error> {
    let fmt = mem.word(field(device, d::pwfx))?;
    Ok(u32::from(mem.short(field(fmt, wave::nChannels))?))
}
fn audible(mem: &mut Memory<'_>, buffer: u32, channels: u32) -> Result<bool, Error> {
    for c in 0..channels {
        if mem.word(field(
            buffer,
            b::volpan + v::dwTotalAmpFactor + c as usize * 4,
        ))? != 0
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn preflight(mem: &mut Memory<'_>, prepared: Prepared, mix: &Mix) -> Result<bool, Error> {
    let count = mem.word(field(mix.device, d::nrofbuffers))? as i32;
    for i in 0..count {
        let list = mem.word(field(mix.device, d::buffers))?;
        let buffer = mem.word(list.wrapping_add(i as u32 * 4))?;
        if mem.word(field(buffer, b::buflen))? == 0 || mem.word(field(buffer, b::state))? == 0 {
            continue;
        }
        let lock = mem.word(field(buffer, b::lock))?;
        if lock & 0xffff != 0 {
            return Ok(true);
        }
        if lock >> 16 == 0 && mem.word(prepared.nt.queue(field(buffer, b::lock) + 2) + 8)? != 0 {
            return Ok(true);
        }
        let device = mem.word(field(buffer, b::device))?;
        if mix
            .frames
            .wrapping_mul(channels(mem, device)?)
            .wrapping_mul(4)
            != 0
        {
            let n = mem.word(field(buffer, b::num_filters))? as i32;
            for i in 0..n {
                let filters = mem.word(field(buffer, b::filters))?;
                if mem.word(field(filters.wrapping_add(i as u32 * 24), filter::inplace))? != 0 {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

// spec: NAT-BODY-3
pub(super) fn mix(env: &mut Env<'_>) -> Outcome {
    let args = [
        env.cpu.reg(Reg::Eax),
        env.cpu.reg(Reg::Edx),
        env.cpu.reg(Reg::Ecx),
        env.arg(0),
    ];
    let Some(prepared) = env.ext.try_get::<Prepared>().copied() else {
        return Outcome::Raise(Fault {
            kind: FaultKind::Read(args[0]),
            pc: env.cpu.reg(Reg::Eip),
        });
    };
    let fp = match Float::new(env.cpu) {
        Ok(fp) => fp,
        Err(error) => return Outcome::Fail(error),
    };
    let state = Mix {
        device: args[0],
        output: args[1],
        frames: args[2],
        stopped: args[3],
        index: 0,
        buffer: 0,
        oldpos: 0,
        advance: 0,
        acc: 0,
        cache: 0,
        required: 0,
        phase: Phase::Buffer,
        fp,
        notify: 0,
        notify_phase: 0,
        first: 0,
        left: 0,
    };
    let result = preflight(&mut Memory::new(env.cpu), prepared, &state);
    match result {
        Ok(true) => {
            env.ext.get_or_default::<super::Declines>().0 += 1;
            super::resume_original(env)
        }
        Ok(false) => {
            if let Err(error) = Memory::new(env.cpu).set(state.stopped, 1) {
                return fail(env, prepared, state, error);
            }
            run(env, prepared, state)
        }
        Err(error) => fail(env, prepared, state, error),
    }
}
fn fail(env: &mut Env<'_>, prepared: Prepared, state: Mix, error: Error) -> Outcome {
    state.fp.publish(env.cpu, prepared.entry);
    match error {
        Error::Fault(kind) => Outcome::Raise(Fault {
            kind,
            pc: prepared.entry,
        }),
        Error::Helper(at) => Outcome::Fail(format!("unknown mixer helper {at:#x}")),
    }
}
fn run(env: &mut Env<'_>, prepared: Prepared, mut state: Mix) -> Outcome {
    loop {
        let result = execute(&mut Memory::new(env.cpu), prepared, &mut state);
        match result {
            Ok(Step::Done) => {
                state.fp.publish(env.cpu, prepared.entry);
                return Outcome::Return(0);
            }
            Ok(Step::Call(call)) => {
                state.fp.publish(env.cpu, prepared.entry);
                return Outcome::call(
                    call.target,
                    call.args[..call.argc].to_vec(),
                    move |env, result| {
                        let mut memory = Memory::new(env.cpu);
                        let result = memory.set(call.pointer, result.value()).and_then(|()| {
                            if let Some((at, len)) = call.length {
                                memory.set(at, len)?;
                            }
                            Ok(())
                        });
                        match result {
                            Ok(()) => run(env, prepared, state),
                            Err(error) => fail(env, prepared, state, error),
                        }
                    },
                );
            }
            Ok(Step::Notify(handle)) => match crate::sync::set_audio_event(env, handle) {
                Ok(()) => (),
                Err(crate::status::Error::Status(status)) => {
                    let teb = env.sys.contexts[env.context as usize].teb;
                    let mut memory = Memory::new(env.cpu);
                    let result = prepared.nt.error(&mut memory, status).and_then(|error| {
                        memory.set(teb + 0xbf4, status)?;
                        memory.set(teb + adv_machine::layout::teb::LAST_ERROR, error)
                    });
                    if let Err(error) = result {
                        return fail(env, prepared, state, error);
                    }
                }
                Err(crate::status::Error::Unsupported(error)) => return Outcome::Fail(error),
            },
            Ok(Step::Wake(tid)) => {
                let _ = crate::sync::alert_thread(env, &[tid]);
            }
            Ok(Step::WakeGuest(address)) => {
                state.fp.publish(env.cpu, prepared.entry);
                return Outcome::call(prepared.nt.wake, vec![address], move |env, _| {
                    run(env, prepared, state)
                });
            }
            Err(error) => return fail(env, prepared, state, error),
        }
    }
}

fn execute(mem: &mut Memory<'_>, prepared: Prepared, state: &mut Mix) -> Result<Step, Error> {
    loop {
        let buffer = state.buffer;
        match state.phase {
            Phase::Buffer => {
                let count = mem.word(field(state.device, d::nrofbuffers))? as i32;
                if state.index >= count {
                    return Ok(Step::Done);
                }
                let list = mem.word(field(state.device, d::buffers))?;
                state.buffer = mem.word(list.wrapping_add(state.index as u32 * 4))?;
                let buffer = state.buffer;
                if mem.word(field(buffer, b::buflen))? == 0
                    || mem.word(field(buffer, b::state))? == 0
                {
                    state.index += 1;
                    continue;
                }
                let lock = mem.word(field(buffer, b::lock))?;
                mem.set(field(buffer, b::lock), lock.wrapping_add(0x10000))?;
                if mem.word(field(buffer, b::state))? == 0 {
                    state.phase = Phase::Unlock;
                    continue;
                }
                if mem.word(field(buffer, b::state))? == 1 {
                    mem.set(field(buffer, b::state), 2)?;
                }
                state.oldpos = mem.word(field(buffer, b::sec_mixpos))?;
                state.phase = Phase::Temp;
            }
            Phase::Temp => {
                let device = mem.word(field(buffer, b::device))?;
                let size = state
                    .frames
                    .wrapping_mul(channels(mem, device)?)
                    .wrapping_mul(4);
                let pointer = mem.word(field(device, d::tmp_buffer))?;
                state.phase = Phase::Ready;
                if pointer == 0 || mem.word(field(device, d::tmp_buffer_len))? < size {
                    mem.set(field(device, d::tmp_buffer_len), size)?;
                    return Ok(Step::Call(Call {
                        target: mem.word(prepared.realloc)?,
                        args: [pointer, size],
                        argc: 2,
                        pointer: field(device, d::tmp_buffer),
                        length: None,
                    }));
                }
            }
            Phase::Ready => {
                let device = mem.word(field(buffer, b::device))?;
                if mem.word(field(buffer, b::put_aux))? == prepared.put[1] {
                    let pointer = mem.word(field(device, d::tmp_buffer))?;
                    let len = mem.word(field(device, d::tmp_buffer_len))?;
                    mem.fill(pointer, len)?;
                }
                let num = mem.long(field(buffer, b::freqAdjustNum))?;
                let den = mem.long(field(buffer, b::freqAdjustDen))?;
                state.acc = mem.long(field(buffer, b::freqAccNum))?;
                state.advance = state.frames;
                state.cache = 0;
                state.required = state.frames;
                state.phase = Phase::Convert;
                if num != den {
                    let end = state
                        .acc
                        .wrapping_add(i64::from(state.frames).wrapping_mul(num));
                    if den == 0 {
                        return Err(Error::Fault(FaultKind::IntegerDivideByZero));
                    }
                    state.advance = end.wrapping_div(den) as u32;
                    let step = mem.word(field(buffer, b::firstep))?;
                    if step == 0 {
                        return Err(Error::Fault(FaultKind::IntegerDivideByZero));
                    }
                    state.cache = (FIR_LEN as u32).wrapping_add(step).wrapping_sub(2) / step;
                    state.required = state.advance.wrapping_add(state.cache);
                    mem.write(
                        field(buffer, b::freqAccNum),
                        end.wrapping_rem(den).to_le_bytes(),
                    )?;
                    let count = channels(mem, device)?;
                    if audible(mem, buffer, count)? {
                        state.phase = Phase::Cache;
                    }
                }
            }
            Phase::Cache => {
                let device = mem.word(field(buffer, b::device))?;
                let count = mem.word(field(buffer, b::mix_channels))?;
                let len = state
                    .required
                    .wrapping_mul(count)
                    .wrapping_add(state.cache)
                    .wrapping_mul(4);
                let pointer = mem.word(field(device, d::cp_buffer))?;
                state.phase = Phase::Convert;
                if pointer == 0 || len > mem.word(field(device, d::cp_buffer_len))? {
                    let (target, args, argc) = if pointer == 0 {
                        (mem.word(prepared.malloc)?, [len, 0], 1)
                    } else {
                        (mem.word(prepared.realloc)?, [pointer, len], 2)
                    };
                    return Ok(Step::Call(Call {
                        target,
                        args,
                        argc,
                        pointer: field(device, d::cp_buffer),
                        length: Some((field(device, d::cp_buffer_len), len)),
                    }));
                }
            }
            Phase::Convert => {
                convert(mem, prepared, state)?;
                state.phase = Phase::Positions;
            }
            Phase::Positions => {
                let fmt = mem.word(field(buffer, b::pwfx))?;
                let stride = u32::from(mem.short(field(fmt, wave::nBlockAlign))?);
                let len = mem.word(field(buffer, b::buflen))?;
                let mut position = mem
                    .word(field(buffer, b::sec_mixpos))?
                    .wrapping_add(state.advance.wrapping_mul(stride));
                if position >= len {
                    if mem.word(field(buffer, b::playflags))? & 1 != 0 {
                        position %= len;
                    } else {
                        position = 0;
                        mem.set(field(buffer, b::state), 0)?;
                    }
                }
                mem.set(field(buffer, b::sec_mixpos), position)?;
                if mem.word(field(buffer, b::use_committed))? != 0 {
                    let position = mem
                        .word(field(buffer, b::committed_mixpos))?
                        .wrapping_add(state.advance.wrapping_mul(stride));
                    mem.set(field(buffer, b::committed_mixpos), position)?;
                    if position >= mem.word(field(buffer, b::writelead))? {
                        mem.set(field(buffer, b::use_committed), 0)?;
                    }
                }
                volume_and_mix(mem, prepared, state)?;
                state.phase = Phase::Notify;
                state.notify = 0;
                state.notify_phase = 0;
            }
            Phase::Notify => {
                if let Some(handle) = notification(mem, state)? {
                    return Ok(Step::Notify(handle));
                }
                mem.set(state.stopped, 0)?;
                state.phase = Phase::Unlock;
            }
            Phase::Unlock => {
                let lock = mem.word(field(buffer, b::lock))?;
                mem.set(field(buffer, b::lock), lock.wrapping_sub(0x10000))?;
                state.index += 1;
                state.phase = Phase::Buffer;
                if lock.wrapping_sub(0x10000) >> 16 == 0 {
                    let address = field(buffer, b::lock) + 2;
                    if mem.word(prepared.nt.queue(address) + 8)? != 0 {
                        return Ok(Step::WakeGuest(address));
                    }
                    if let Some(tid) = prepared.nt.wake(mem, address)? {
                        return Ok(Step::Wake(tid));
                    }
                }
            }
        }
    }
}

fn get(
    mem: &mut Memory<'_>,
    prepared: Prepared,
    fp: &mut Float,
    buffer: u32,
    at: u32,
    channel: u32,
    function: u32,
) -> Result<f32, Error> {
    let kind = prepared
        .get
        .iter()
        .position(|&p| p == function)
        .ok_or(Error::Helper(function))?;
    let value = match kind {
        0 => {
            let n = i32::from(mem.read::<1>(at.wrapping_add(channel))?[0]) - 128;
            let n = fp.int(n)?;
            fp.mul(n, 1.0 / 128.0)?
        }
        1 => {
            let n = mem.short(at.wrapping_add(channel.wrapping_mul(2)))? as i16;
            let n = fp.int(i32::from(n))?;
            fp.mul(n, 1.0 / 32768.0)?
        }
        2 => {
            let bytes = mem.read::<3>(at.wrapping_add(channel.wrapping_mul(3)))?;
            let n = i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]);
            let n = fp.int(n)?;
            fp.mul(n, 1.0 / 2147483648.0)?
        }
        3 => {
            let n = mem.word(at.wrapping_add(channel.wrapping_mul(4)))? as i32;
            let n = fp.int(n)?;
            fp.mul(n, 1.0 / 2147483648.0)?
        }
        4 => mem.float(at.wrapping_add(channel.wrapping_mul(4)))?,
        5 => {
            let format = mem.word(field(buffer, b::pwfx))?;
            let count = u32::from(mem.short(field(format, wave::nChannels))?);
            let mut sum = 0.0;
            for c in 0..count {
                let function = mem.word(field(buffer, b::get_aux))?;
                let value = get(mem, prepared, fp, buffer, at, c, function)?;
                sum = fp.add(sum, value)?;
            }
            let count = fp.int(count as i32)?;
            fp.div(sum, count)?
        }
        _ => unreachable!(),
    };
    fp.returned(value)
}
fn sample(
    mem: &mut Memory<'_>,
    prepared: Prepared,
    state: &mut Mix,
    i: u32,
    c: u32,
    committed: u32,
    stride: u32,
) -> Result<f32, Error> {
    let buffer = state.buffer;
    let (source, len, pos) = if i < committed {
        (
            mem.word(field(buffer, b::committedbuff))?,
            mem.word(field(buffer, b::writelead))?,
            mem.word(field(buffer, b::committed_mixpos))?
                .wrapping_add(i.wrapping_mul(stride)),
        )
    } else {
        let storage = mem.word(field(buffer, b::buffer))?;
        (
            mem.word(field(storage, memory::memory))?,
            mem.word(field(buffer, b::buflen))?,
            mem.word(field(buffer, b::sec_mixpos))?
                .wrapping_add(i.wrapping_mul(stride)),
        )
    };
    if pos >= len && mem.word(field(buffer, b::playflags))? & 1 == 0 {
        return Ok(0.0);
    }
    if len == 0 {
        return Err(Error::Fault(FaultKind::IntegerDivideByZero));
    }
    let function = mem.word(field(buffer, b::get))?;
    get(
        mem,
        prepared,
        &mut state.fp,
        buffer,
        source.wrapping_add(pos % len),
        c,
        function,
    )
}
#[allow(
    clippy::too_many_arguments,
    reason = "Guest callback arguments and its memory/FP contexts"
)]
fn put(
    mem: &mut Memory<'_>,
    prepared: Prepared,
    fp: &mut Float,
    buffer: u32,
    pos: u32,
    c: u32,
    value: f32,
    function: u32,
) -> Result<(), Error> {
    let kind = prepared
        .put
        .iter()
        .position(|&p| p == function)
        .ok_or(Error::Helper(function))?;
    if kind <= 1 {
        let device = mem.word(field(buffer, b::device))?;
        let tmp = mem.word(field(device, d::tmp_buffer))?;
        let at = tmp.wrapping_add(pos).wrapping_add(c.wrapping_mul(4));
        let value = if kind == 1 {
            let old = mem.float(at)?;
            fp.add(old, value)?
        } else {
            value
        };
        return mem.set_float(at, value);
    }
    let aux = mem.word(field(buffer, b::put_aux))?;
    let mut send = |channel, value| put(mem, prepared, fp, buffer, pos, channel, value, aux);
    match kind {
        2..=4 => {
            for channel in 0..[2, 4, 6][kind - 2] {
                send(channel, value)?;
            }
        }
        5 if c <= 1 => {
            send(c, value)?;
            send(c + 2, value)?;
        }
        6 if c == 0 => {
            send(0, value)?;
            send(4, value)?;
            send(2, 0.0)?;
            send(3, 0.0)?;
        }
        6 if c == 1 => {
            send(1, value)?;
            send(5, value)?;
        }
        7 if c < 4 => {
            let value = fp.mul(value, if c < 2 { 0.9 } else { 0.1 })?;
            put(mem, prepared, fp, buffer, pos, c % 2, value, aux)?;
        }
        8 | 9 => match c {
            0 | 1 => put(mem, prepared, fp, buffer, pos, c, value, aux)?,
            2 => {
                let wide = fp.widen(value)?;
                let wide = fp.double(wide, 0.7, 2)?;
                let value = fp.narrow(wide)?;
                put(mem, prepared, fp, buffer, pos, 0, value, aux)?;
                put(mem, prepared, fp, buffer, pos, 1, value, aux)?;
            }
            4 | 5 => {
                let value = fp.mul(value, 0.24)?;
                put(mem, prepared, fp, buffer, pos, c % 2, value, aux)?;
            }
            6 | 7 if kind == 9 => {
                let value = fp.mul(value, 0.24)?;
                put(mem, prepared, fp, buffer, pos, c % 2, value, aux)?;
            }
            _ => (),
        },
        _ => (),
    }
    Ok(())
}
fn convert(mem: &mut Memory<'_>, prepared: Prepared, state: &mut Mix) -> Result<(), Error> {
    let buffer = state.buffer;
    let device = mem.word(field(buffer, b::device))?;
    let outputs = channels(mem, device)?;
    if !audible(mem, buffer, outputs)? {
        return Ok(());
    }
    let fmt = mem.word(field(buffer, b::pwfx))?;
    let stride = u32::from(mem.short(field(fmt, wave::nBlockAlign))?);
    let count = mem.word(field(buffer, b::mix_channels))?;
    let committed = if mem.word(field(buffer, b::use_committed))? != 0 {
        if stride == 0 {
            return Err(Error::Fault(FaultKind::IntegerDivideByZero));
        }
        (mem.word(field(buffer, b::writelead))?
            .wrapping_sub(mem.word(field(buffer, b::committed_mixpos))?)
            / stride)
            .min(state.required)
    } else {
        0
    };
    let resample =
        mem.long(field(buffer, b::freqAdjustNum))? != mem.long(field(buffer, b::freqAdjustDen))?;
    if !resample {
        for i in 0..state.frames {
            for c in 0..count {
                let value = sample(mem, prepared, state, i, c, committed, stride)?;
                let function = mem.word(field(buffer, b::put))?;
                put(
                    mem,
                    prepared,
                    &mut state.fp,
                    buffer,
                    i.wrapping_mul(outputs).wrapping_mul(4),
                    c,
                    value,
                    function,
                )?;
            }
        }
        return Ok(());
    }
    let cp = mem.word(field(device, d::cp_buffer))?;
    for c in 0..count {
        for i in 0..state.required {
            let value = sample(mem, prepared, state, i, c, committed, stride)?;
            mem.set_float(
                cp.wrapping_add(
                    state
                        .cache
                        .wrapping_add(c.wrapping_mul(state.required))
                        .wrapping_add(i)
                        .wrapping_mul(4),
                ),
                value,
            )?;
        }
    }
    let den = mem.long(field(buffer, b::freqAdjustDen))?;
    let num = mem.long(field(buffer, b::freqAdjustNum))?;
    let step = mem.word(field(buffer, b::firstep))?;
    for i in 0..state.frames {
        let steps = state
            .acc
            .wrapping_add(i64::from(i).wrapping_mul(num))
            .wrapping_mul(i64::from(step));
        let integer = steps.wrapping_div(den) as u32;
        let input = integer / step;
        let numerator = state.fp.long(steps)?;
        let denominator = state.fp.long(den)?;
        let total = state.fp.div(numerator, denominator)?;
        let total = state.fp.widen(total)?;
        let rem = state.fp.double(f64::from(integer), 1.0, 0)?;
        let rem = state.fp.double(rem, total, 1)?;
        let rem = state.fp.narrow(rem)?;
        let mut idx = input
            .wrapping_add(1)
            .wrapping_mul(step)
            .wrapping_sub(integer)
            .wrapping_sub(1);
        let mut used = 0u32;
        while idx < FIR_LEN as u32 - 1 {
            let a = mem.float(prepared.fir.wrapping_add(idx.wrapping_mul(4)))?;
            let z = mem.float(
                prepared
                    .fir
                    .wrapping_add(idx.wrapping_add(1).wrapping_mul(4)),
            )?;
            let wide_rem = state.fp.widen(rem)?;
            let one = state.fp.double(1.0, wide_rem, 1)?;
            let a = state.fp.widen(a)?;
            let a = state.fp.double(a, one, 2)?;
            let z = state.fp.mul(z, rem)?;
            let z = state.fp.widen(z)?;
            let result = state.fp.double(z, a, 0)?;
            let result = state.fp.narrow(result)?;
            mem.set_float(cp.wrapping_add(used.wrapping_mul(4)), result)?;
            used = used.wrapping_add(1);
            idx = idx.wrapping_add(mem.word(field(buffer, b::firstep))?);
        }
        for c in 0..mem.word(field(buffer, b::mix_channels))? {
            let mut sum = 0.0;
            for j in 0..used {
                let sample = mem.float(
                    cp.wrapping_add(
                        state
                            .cache
                            .wrapping_add(c.wrapping_mul(state.required))
                            .wrapping_add(input)
                            .wrapping_add(j)
                            .wrapping_mul(4),
                    ),
                )?;
                let coefficient = mem.float(cp.wrapping_add(j.wrapping_mul(4)))?;
                let product = state.fp.mul(sample, coefficient)?;
                sum = state.fp.add(sum, product)?;
            }
            let gain = mem.float(field(buffer, b::firgain))?;
            let value = state.fp.mul(sum, gain)?;
            let function = mem.word(field(buffer, b::put))?;
            put(
                mem,
                prepared,
                &mut state.fp,
                buffer,
                i.wrapping_mul(outputs).wrapping_mul(4),
                c,
                value,
                function,
            )?;
        }
    }
    Ok(())
}
fn volume_and_mix(mem: &mut Memory<'_>, _prepared: Prepared, state: &mut Mix) -> Result<(), Error> {
    let buffer = state.buffer;
    let device = mem.word(field(buffer, b::device))?;
    let channels = channels(mem, device)?;
    if !audible(mem, buffer, channels)? {
        return Ok(());
    }
    let tmp = mem.word(field(device, d::tmp_buffer))?;
    let flags = mem.word(field(buffer, b::dsbd + desc::dwFlags))?;
    let volume = flags & 0x10 != 0
        || (flags & 0x80 != 0 && mem.word(field(buffer, b::volpan + v::lVolume))? != 0)
        || (flags & 0x40 != 0 && mem.word(field(buffer, b::volpan + v::lPan))? != 0);
    if volume && channels <= 6 {
        let mut factors = [0.0; 6];
        for c in 0..channels {
            let value = mem.word(field(
                buffer,
                b::volpan + v::dwTotalAmpFactor + c as usize * 4,
            ))?;
            let hi = state.fp.int((value >> 16) as i32)?;
            let hi = state.fp.mul(hi, 65536.0)?;
            let lo = state.fp.int((value & 65535) as i32)?;
            let value = state.fp.add(hi, lo)?;
            factors[c as usize] = state.fp.div(value, 65535.0)?;
        }
        for i in 0..state.frames as i32 {
            for c in 0..channels {
                let at = tmp.wrapping_add(
                    (i as u32)
                        .wrapping_mul(channels)
                        .wrapping_add(c)
                        .wrapping_mul(4),
                );
                let value = mem.float(at)?;
                let value = state.fp.mul(value, factors[c as usize])?;
                mem.set_float(at, value)?;
            }
        }
    }
    for i in 0..state.frames.wrapping_mul(channels) {
        let src = mem.float(tmp.wrapping_add(i.wrapping_mul(4)))?;
        let dst = mem.float(state.output.wrapping_add(i.wrapping_mul(4)))?;
        let value = state.fp.add(src, dst)?;
        mem.set_float(state.output.wrapping_add(i.wrapping_mul(4)), value)?;
    }
    Ok(())
}
fn notification(mem: &mut Memory<'_>, state: &mut Mix) -> Result<Option<u32>, Error> {
    let buffer = state.buffer;
    if mem.word(field(buffer, b::dsbd + desc::dwFlags))? & 0x100 == 0 {
        return Ok(None);
    }
    let count = mem.word(field(buffer, b::nrofnotifies))? as i32;
    if count <= 0 {
        return Ok(None);
    }
    let array = mem.word(field(buffer, b::notifies))?;
    let offset = |mem: &mut Memory<'_>, i: i32| {
        mem.word(field(
            array.wrapping_add((i as u32).wrapping_mul(8)),
            notify::dwOffset,
        ))
    };
    let len = mem.word(field(buffer, b::buflen))?;
    let new = mem.word(field(buffer, b::sec_mixpos))?;
    let end = state.oldpos.wrapping_add(if new >= state.oldpos {
        new - state.oldpos
    } else {
        len.wrapping_add(new).wrapping_sub(state.oldpos)
    });
    loop {
        match state.notify_phase {
            0 => {
                if mem.word(field(buffer, b::state))? == 0
                    && state.notify < count
                    && offset(mem, state.notify)? == u32::MAX
                {
                    let i = state.notify;
                    state.notify += 1;
                    return Ok(Some(mem.word(array.wrapping_add(i as u32 * 8 + 4))?));
                }
                state.notify_phase = 1;
                state.notify = 0;
            }
            1 => {
                let mut first = 0;
                while first < count && offset(mem, first)? == u32::MAX {
                    first += 1;
                }
                if first == count {
                    return Ok(None);
                }
                state.first = first;
                let mut left = first;
                let mut right = count - 1;
                while left != right {
                    let check = left + (right - left) / 2;
                    match offset(mem, check)?.cmp(&state.oldpos) {
                        std::cmp::Ordering::Less => left = check + 1,
                        std::cmp::Ordering::Greater => right = check,
                        std::cmp::Ordering::Equal => {
                            left = check;
                            break;
                        }
                    }
                }
                state.left = left;
                state.notify = left;
                state.notify_phase = 2;
                if offset(mem, left)? < state.oldpos {
                    state.notify = count;
                }
            }
            2 => {
                if state.notify < count && offset(mem, state.notify)? < end {
                    let i = state.notify;
                    state.notify += 1;
                    return Ok(Some(mem.word(array.wrapping_add(i as u32 * 8 + 4))?));
                }
                if end <= len {
                    return Ok(None);
                }
                state.notify_phase = 3;
                state.notify = state.first;
            }
            _ => {
                if state.notify >= state.left || offset(mem, state.notify)? >= end % len {
                    return Ok(None);
                }
                let i = state.notify;
                state.notify += 1;
                return Ok(Some(mem.word(array.wrapping_add(i as u32 * 8 + 4))?));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::tests::{ALLOCATIONS, COUNTING};
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    // spec: NAT-BODY-3, NAT-BODY-15, NAT-BODY-17
    #[test]
    fn mixer_in_place_work_does_not_allocate() {
        let mut machine = Machine::new(
            adv_cpu::Cpu::new(PARK_ADDR, false).unwrap(),
            crate::dispatch::Platform,
            Clock::virtual_time(),
        )
        .unwrap();
        let context = machine
            .spawn("mixer allocation gate", PARK_ADDR, &[], 0x10000)
            .unwrap();
        let base = 0x100000;
        machine.cpu.map(base, 0x20000, Prot::RW).unwrap();
        let prepared = Prepared {
            entry: 0x400000,
            fir: base + 0x10000,
            get: [1, 2, 3, 4, 5, 6],
            put: [7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            malloc: 0,
            realloc: 0,
            nt: Nt {
                queues: base + 0xe000,
                wake: 0,
                table: 0,
                map: 0,
                high: 0,
            },
        };
        machine.ext.insert(prepared);
        let buffer = base + 0x1000;
        for (at, value) in [
            (field(base, d::pwfx), base + 0x200),
            (field(base, d::nrofbuffers), 1),
            (field(base, d::buffers), base + 0x300),
            (base + 0x300, buffer),
            (field(base, d::tmp_buffer), base + 0x3000),
            (field(base, d::tmp_buffer_len), 0x1000),
            (field(base, d::cp_buffer), base + 0x4000),
            (field(base, d::cp_buffer_len), 0x8000),
            (field(base + 0x200, wave::nChannels), 2),
            (field(buffer, b::device), base),
            (field(buffer, b::pwfx), base + 0x200),
            (field(base + 0x200, wave::nBlockAlign), 8),
            (field(buffer, b::buffer), base + 0x500),
            (field(base + 0x500, memory::memory), base + 0x2000),
            (field(buffer, b::buflen), 0x1000),
            (field(buffer, b::playflags), 1),
            (field(buffer, b::volpan), 60000),
            (field(buffer, b::volpan + 4), 45000),
            (field(buffer, b::dsbd + desc::dwFlags), 0x10),
            (field(buffer, b::mix_channels), 2),
            (field(buffer, b::put), 7),
            (field(buffer, b::put_aux), 7),
            (field(buffer, b::firstep), 120),
            (field(buffer, b::firgain), 1.0f32.to_bits()),
        ] {
            machine.cpu.write_u32(at, value).unwrap();
        }
        machine
            .cpu
            .write(field(buffer, b::freqAdjustDen), &48000i64.to_le_bytes())
            .unwrap();
        for at in (base + 0x10000..base + 0x10000 + FIR_LEN as u32 * 4).step_by(4) {
            machine.cpu.write_u32(at, 0.125f32.to_bits()).unwrap();
        }
        for getter in prepared.get {
            machine
                .cpu
                .write_u32(field(buffer, b::get), getter)
                .unwrap();
            machine.cpu.write_u32(field(buffer, b::get_aux), 2).unwrap();
            for num in [48000i64, 24000, 96000] {
                machine
                    .cpu
                    .write(field(buffer, b::freqAdjustNum), &num.to_le_bytes())
                    .unwrap();
                for repeat in 0..2 {
                    machine.cpu.write_u32(field(buffer, b::state), 2).unwrap();
                    machine.cpu.set_reg(Reg::Eax, base);
                    machine.cpu.set_reg(Reg::Edx, base + 0xc000);
                    machine.cpu.set_reg(Reg::Ecx, 8);
                    machine.cpu.set_reg(Reg::Esp, base + 0xf000);
                    machine.cpu.write_u32(base + 0xf004, base + 0xd000).unwrap();
                    let mut env = Env {
                        cpu: &mut machine.cpu,
                        sys: &mut machine.sys,
                        ext: &mut machine.ext,
                        context,
                        thunk: 0,
                    };
                    ALLOCATIONS.set(0);
                    COUNTING.set(repeat == 1);
                    let result = mix(&mut env);
                    COUNTING.set(false);
                    assert!(matches!(result, Outcome::Return(0)));
                    assert_eq!(ALLOCATIONS.get(), 0);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Nt {
    queues: u32,
    wake: u32,
    table: u32,
    map: u32,
    high: i32,
}
fn nt_inputs(
    cpu: &dyn Guest,
    module: &crate::identity::Module,
    checks: &mut Vec<adv_cpu::backend::CodeCheck>,
) -> Result<Nt, String> {
    use iced_x86::{Code, Decoder, DecoderOptions, Register};
    let table = module.table().ok_or("missing ntdll identity")?;
    let reloc = relocations(cpu, module.base).ok_or("missing ntdll relocations")?;
    const ERROR_HELPER: &str = concat!("RtlNtStatusToDosErrorNoTeb", "@4.part.0");
    let mut nt = Nt {
        queues: 0,
        wake: 0,
        table: 0,
        map: 0,
        high: 0,
    };
    for name in [
        "RtlWakeAddressSingle",
        "RtlAcquireSRWLockShared",
        "RtlReleaseSRWLockShared",
        ERROR_HELPER,
    ] {
        let f = table.named(name).ok_or("missing ntdll mixer helper")?;
        let at = module.base + f.rva;
        if !crate::identity::intact(cpu, at, f, |at| reloc.binary_search(&at).is_ok()) {
            return Err(format!("modified mixer helper {name}"));
        }
        let mut bytes = vec![0; f.size as usize];
        cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
        for i in Decoder::with_ip(32, &bytes, u64::from(at), DecoderOptions::NONE) {
            let address = i.memory_displacement64() as u32;
            let global = (module.base..module.base + module.size).contains(&address);
            if name == "RtlWakeAddressSingle" {
                nt.wake = at;
                if global && i.code() == Code::Mov_r32_rm32 && i.op0_register() == Register::EAX {
                    nt.queues = address;
                }
            } else if name == ERROR_HELPER {
                if global && i.code() == Code::Mov_r32_rm32 {
                    match i.op0_register() {
                        Register::EDI => nt.table = address,
                        Register::EAX => nt.map = address,
                        _ => (),
                    }
                }
                if i.code() == Code::Mov_r32_imm32 && i.op0_register() == Register::EBX {
                    nt.high = i.immediate32() as i32;
                }
            }
        }
        checks.push((at, bytes.into_boxed_slice()));
    }
    if nt.queues == 0 || nt.table == 0 || nt.map == 0 {
        return Err("missing ntdll mixer data".into());
    }
    Ok(nt)
}
impl Nt {
    fn queue(self, address: u32) -> u32 {
        self.queues.wrapping_add(((address >> 4) & 255) * 12)
    }
    fn wake(self, mem: &mut Memory<'_>, address: u32) -> Result<Option<u32>, Error> {
        let queue = self.queue(address);
        mem.set(queue + 8, u32::MAX)?;
        let mut entry = mem.word(queue)?;
        let mut tid = None;
        if entry == 0 {
            mem.set(queue + 4, queue)?;
            mem.set(queue, queue)?;
        } else {
            while entry != queue {
                let next = mem.word(entry)?;
                if mem.word(entry + 8)? == address {
                    let id = mem.word(entry + 12)?;
                    let previous = mem.word(entry + 4)?;
                    mem.set(entry + 8, 0)?;
                    mem.set(next + 4, previous)?;
                    mem.set(previous, next)?;
                    tid = (id != 0).then_some(id);
                    break;
                }
                entry = next;
            }
        }
        mem.set(queue + 8, 0)?;
        Ok(tid)
    }
    fn error(self, mem: &mut Memory<'_>, mut status: u32) -> Result<u32, Error> {
        if status == 0 || status & 0x20000000 != 0 {
            return Ok(status);
        }
        if status & 0xf0000000 == 0xd0000000 {
            status &= !0x10000000;
        }
        if matches!(status >> 16, 0xc001 | 0x8007 | 0xc007) {
            return Ok(status & 65535);
        }
        let (mut low, mut high) = (0, self.high);
        while low <= high {
            let pos = (low + high) / 2;
            let at = self.table.wrapping_add(pos as u32 * 12);
            let start = mem.word(at)?;
            if status < start {
                high = pos - 1;
            } else if status > mem.word(at + 4)? {
                low = pos + 1;
            } else {
                let offset = mem.word(at + 8)?.wrapping_add(status - start);
                return mem.word(self.map.wrapping_add(offset.wrapping_mul(4)));
            }
        }
        Ok(317)
    }
}
