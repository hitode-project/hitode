//! Admission and execution of the reviewed Vorbis inverse MDCT ABIs.
use std::collections::BTreeMap;
use std::time::Instant;

use adv_abi::code_signature::{self, ExecutableRange, Match, Signature};
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_device::codec_kernel::ImdctCache;
use adv_machine::{Conv, Env, Ext, HostFn, Outcome};

use super::{Entry, Mode};

/// Reviewed signatures shipped with the runtime; fingerprints only, no code bytes.
pub const BUNDLED_SIGNATURES: &str = concat!(
    include_str!("../../../../data/codec/vorbis-mdct-x86.txt"),
    "\n---\n",
    include_str!("../../../../data/codec/vorbis-mdct-sse-x86.txt"),
);

const MAX_BINDINGS: usize = 64;
const MAX_CODE_BYTES: usize = 16 * 1024;
const KERNEL: &str = "vorbis-mdct-backward-f32-cdecl32";
const REGISTER_KERNEL: &str = "vorbis-mdct-backward-f32-register32";
const BINDING_MODULE: &str = "adv-codec-kernel";
// Reviewed library-build contract. The descriptor must state this explicitly;
// SSE rounds each trig argument to f32; x87 retains its extended intermediate.
const TABLE_PI_BITS: u32 = 0x4049_0fdb;

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub theora: super::theora::Stats,
    pub mode: Mode,
    pub installed: u64,
    pub calls: u64,
    pub native_calls: u64,
    pub verify_calls: u64,
    pub fallback_calls: u64,
    pub revocations: u64,
    pub errors: u64,
    pub compared_floats: u64,
    pub different_floats: u64,
    pub nonfinite_floats: u64,
    pub max_abs: f64,
    pub rms: f64,
    pub relative_rms: f64,
    pub total_kernel_ns: u64,
    pub max_kernel_ns: u64,
    pub last_rejection: Option<String>,
    /// First rejected element only; no guest table dump is retained.
    pub table_mismatch: Option<String>,
    /// Keys come only from the fixed rejection vocabulary in this module.
    pub fallback_reasons: BTreeMap<&'static str, u64>,
}

struct Binding {
    module_base: u32,
    matched: Match,
    entry: Entry,
    live: bool,
    revoked: bool,
}

struct StandardTables {
    trig: Vec<f32>,
    bitrev: Vec<i32>,
}
impl StandardTables {
    fn new(n: usize, sse: bool) -> Self {
        let pi = f64::from(f32::from_bits(TABLE_PI_BITS));
        let angle = |value: f64| if sse { f64::from(value as f32) } else { value };
        let mut trig = vec![0.0; n + n / 4];
        for i in 0..n / 4 {
            let a = angle((pi / n as f64) * (4 * i) as f64);
            trig[2 * i] = a.cos() as f32;
            trig[2 * i + 1] = (-a.sin()) as f32;
            let b = angle((pi / (2 * n) as f64) * (2 * i + 1) as f64);
            trig[n / 2 + 2 * i] = b.cos() as f32;
            trig[n / 2 + 2 * i + 1] = b.sin() as f32;
        }
        for i in 0..n / 8 {
            let a = angle((pi / n as f64) * (4 * i + 2) as f64);
            trig[n + 2 * i] = (a.cos() * 0.5) as f32;
            trig[n + 2 * i + 1] = (-a.sin() * 0.5) as f32;
        }
        let log = n.ilog2();
        let mask = (1u32 << (log - 1)) - 1;
        let mut bitrev = Vec::with_capacity(n / 4);
        for i in 0..n / 8 {
            let reversed = (i as u32).reverse_bits() >> (32 - (log - 1));
            bitrev.push((((!reversed) & mask) - 1) as i32);
            bitrev.push(reversed as i32);
        }
        Self { trig, bitrev }
    }
}

struct Provider {
    bindings: Vec<Binding>,
    cache: ImdctCache,
    tables: [[Option<StandardTables>; 8]; 2],
    stats: Stats,
    squared_error: f64,
    squared_reference: f64,
}
impl Provider {
    fn new(mode: Mode) -> Self {
        Self {
            bindings: Vec::new(),
            cache: ImdctCache::default(),
            tables: std::array::from_fn(|_| std::array::from_fn(|_| None)),
            stats: Stats {
                mode,
                ..Stats::default()
            },
            squared_error: 0.0,
            squared_reference: 0.0,
        }
    }
    fn rejection(&mut self, reason: &'static str) {
        self.stats.last_rejection = Some(reason.to_owned());
        *self.stats.fallback_reasons.entry(reason).or_default() += 1;
    }
}

pub(super) fn install(ext: &mut Ext, mode: Mode) {
    ext.insert(Provider::new(mode));
}

pub(super) fn rejected(ext: &mut Ext, reason: String) {
    ext.get_mut::<Provider>().stats.last_rejection = Some(reason);
}

pub(super) fn stats(ext: &Ext) -> Option<Stats> {
    ext.try_get::<Provider>().map(|p| {
        let mut stats = p.stats.clone();
        stats.theora = super::theora::stats(ext);
        stats
    })
}

pub(super) fn host_fn(module: &str, name: &str) -> Option<HostFn> {
    if module != BINDING_MODULE {
        return None;
    }
    let (abi, index) = name.split_once('_')?;
    let argc = match abi {
        "cdecl" => 3,
        "register" => 1,
        _ => return None,
    };
    Some(HostFn {
        conv: Conv::Cdecl { argc },
        env_leaf: Some(native),
        handler: call,
        data: index.parse().ok()?,
        trace: false,
        guest: None,
        leaf: None,
    })
}

pub(super) fn bind(
    e: &mut Env<'_>,
    base: u32,
    size: u32,
    ranges: &[ExecutableRange],
) -> Result<(), String> {
    bind_signatures(e, base, size, ranges, BUNDLED_SIGNATURES)
}

fn bind_signatures(
    e: &mut Env<'_>,
    base: u32,
    size: u32,
    ranges: &[ExecutableRange],
    text: &str,
) -> Result<(), String> {
    let signatures = text
        .split("\n---\n")
        .map(Signature::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for signature in &signatures {
        let properties = &signature.profile.properties;
        let required = [
            ("lookup_bytes", 20),
            ("n_offset", 0),
            ("log2n_offset", 4),
            ("trig_offset", 8),
            ("bitrev_offset", 12),
            ("scale_offset", 16),
            ("min_n", 64),
            ("max_n", 8192),
            ("table_pi_bits", TABLE_PI_BITS),
        ];
        let table = match signature.profile.kernel.as_str() {
            KERNEL => ("table_pi_bits", TABLE_PI_BITS),
            REGISTER_KERNEL => ("table_angle_f32", 1),
            _ => return Err("unsupported codec kernel ABI profile".into()),
        };
        if required
            .iter()
            .chain(std::iter::once(&table))
            .any(|&(key, value)| properties.get(key) != Some(&value))
        {
            return Err("unsupported codec kernel ABI profile".into());
        }
    }
    for signature in &signatures {
        let found = match code_signature::scan(signature, ranges, &mut |address, len| {
            let mut bytes = vec![0; len];
            e.cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
            Ok(bytes)
        }) {
            Ok(found) => found,
            Err(code_signature::Error::NotFound) => continue,
            Err(error) => {
                e.ext.get_mut::<Provider>().stats.last_rejection = Some(error.to_string());
                continue;
            }
        };
        for matched in found {
            let code_bytes: usize = matched.functions.iter().map(|f| f.bytes.len()).sum();
            let complete = matched.functions.iter().all(|f| {
                f.entry
                    .checked_sub(base)
                    .and_then(|offset| offset.checked_add(f.bytes.len() as u32))
                    .is_some_and(|end| end <= size)
            });
            if !(5..=32).contains(&matched.prologue_len)
                || code_bytes > MAX_CODE_BYTES
                || !complete
                || e.ext.get::<Provider>().bindings.len() == MAX_BINDINGS
            {
                continue;
            }
            bind_one(e, base, matched)?;
        }
    }
    Ok(())
}

fn bind_one(e: &mut Env<'_>, base: u32, matched: Match) -> Result<(), String> {
    let index = e.ext.get::<Provider>().bindings.len();
    let abi = if matched.profile.kernel == REGISTER_KERNEL {
        "register"
    } else {
        "cdecl"
    };
    let root = matched
        .functions
        .iter()
        .find(|f| f.entry == matched.entry)
        .ok_or("signature match has no root function")?;
    let entry = super::entry(
        e.cpu,
        e.sys,
        BINDING_MODULE,
        &format!("{abi}_{index}"),
        matched.entry,
        matched.prologue_len,
        &root.bytes,
    )?;
    super::redirect(e.sys, &entry, &root.bytes);
    let provider = e.ext.get_mut::<Provider>();
    provider.bindings.push(Binding {
        module_base: base,
        matched,
        entry,
        live: true,
        revoked: false,
    });
    provider.stats.installed += 1;
    Ok(())
}

pub(super) fn forget(ext: &mut Ext, base: u32) {
    for binding in &mut ext.get_mut::<Provider>().bindings {
        if binding.module_base == base {
            binding.live = false;
        }
    }
}

fn identical(cpu: &dyn Guest, address: u32, expected: &[u8]) -> bool {
    let mut actual = vec![0; expected.len()];
    cpu.read(address, &mut actual).is_ok() && actual == expected
}

fn original(e: &mut Env<'_>, index: usize) -> Outcome {
    let binding = &e.ext.get::<Provider>().bindings[index];
    if !binding.live || !identical(e.cpu, binding.entry.original, &binding.entry.trampoline) {
        e.ext.get_mut::<Provider>().stats.errors += 1;
        return Outcome::Fail("codec kernel original continuation is no longer verified".into());
    }
    super::original(e, binding.entry.original)
}

fn closure_intact(cpu: &dyn Guest, binding: &Binding) -> bool {
    binding.matched.functions.iter().all(|f| {
        cpu.accessible(f.entry, f.bytes.len() as u32, Prot::EXEC)
            && identical(cpu, f.entry, &f.bytes)
    }) && binding.matched.constants.iter().all(|c| {
        cpu.accessible(c.address, c.bytes.len() as u32, Prot::READ)
            && identical(cpu, c.address, &c.bytes)
    })
}

fn span(address: u32, bytes: usize) -> Option<(u32, u32)> {
    Some((address, address.checked_add(u32::try_from(bytes).ok()?)?))
}
fn overlaps(a: (u32, u32), b: (u32, u32)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn ulp_match(actual: f32, expected: f32) -> bool {
    if actual.to_bits() == expected.to_bits() {
        return true;
    }
    actual.is_normal()
        && expected.is_normal()
        && actual.is_sign_negative() == expected.is_sign_negative()
        && actual.to_bits().abs_diff(expected.to_bits()) <= 2
}

fn arguments(e: &Env<'_>, index: usize) -> Result<[u32; 3], &'static str> {
    if e.ext.get::<Provider>().bindings[index]
        .matched
        .profile
        .kernel
        == REGISTER_KERNEL
    {
        Ok([
            e.cpu.reg(Reg::Ecx),
            e.cpu.reg(Reg::Edx),
            e.cpu.arg(0).map_err(|_| "unreadable arguments")?,
        ])
    } else {
        let [Ok(lookup), Ok(source), Ok(output)] = [e.cpu.arg(0), e.cpu.arg(1), e.cpu.arg(2)]
        else {
            return Err("unreadable arguments");
        };
        Ok([lookup, source, output])
    }
}

/// Read fresh table contents on every call. A heap pointer is never a content identity.
fn input(e: &mut Env<'_>, index: usize) -> Result<(u32, Vec<f32>), &'static str> {
    let [lookup, source, output] = arguments(e, index)?;
    let lookup_span = span(lookup, 20).ok_or("lookup range wraps")?;
    if !e.cpu.accessible(lookup, 20, Prot::READ) {
        return Err("unreadable lookup");
    }
    let mut words = [0u8; 20];
    e.cpu
        .read(lookup, &mut words)
        .map_err(|_| "unreadable lookup")?;
    let word = |at| u32::from_le_bytes(words[at..at + 4].try_into().unwrap());
    let n = word(0) as usize;
    if !(64..=8192).contains(&n)
        || !n.is_power_of_two()
        || word(4) != n.ilog2()
        || word(16) != (4.0f32 / n as f32).to_bits()
    {
        return Err("unsupported lookup layout or size");
    }
    let trig = word(8);
    let bitrev = word(12);
    let trig_len = (n + n / 4) * 4;
    let bit_len = n;
    let trig_span = span(trig, trig_len).ok_or("table range wraps")?;
    let bit_span = span(bitrev, bit_len).ok_or("table range wraps")?;
    let source_span = span(source, n * 2).ok_or("input range wraps")?;
    let output_span = span(output, n * 4).ok_or("output range wraps")?;
    if !e.cpu.accessible(source, (n * 2) as u32, Prot::READ)
        || !e.cpu.accessible(output, (n * 4) as u32, Prot::WRITE)
        || !e.cpu.accessible(trig, trig_len as u32, Prot::READ)
        || !e.cpu.accessible(bitrev, bit_len as u32, Prot::READ)
    {
        return Err("inaccessible kernel buffers");
    }
    // Address-disjoint buffers are only physically disjoint for ordinary
    // private storage. This borrow probe rejects device/file/shared overlays
    // and unsupported backends; no slice survives the probe or guest execution.
    if [
        (lookup, 20),
        (source, n * 2),
        (output, n * 4),
        (trig, trig_len),
        (bitrev, bit_len),
    ]
    .into_iter()
    .any(|(address, len)| e.cpu.shared_bytes(address, len).is_none())
    {
        return Err("nonordinary kernel storage");
    }
    if source != output && overlaps(source_span, output_span) {
        return Err("partial input output alias");
    }
    if [lookup_span, trig_span, bit_span]
        .into_iter()
        .any(|s| overlaps(s, output_span))
        || span(e.cpu.reg(Reg::Esp), 16).is_none_or(|s| overlaps(s, output_span))
    {
        return Err("output aliases kernel metadata or caller frame");
    }
    let binding = &e.ext.get::<Provider>().bindings[index];
    if binding
        .matched
        .functions
        .iter()
        .any(|f| span(f.entry, f.bytes.len()).is_none_or(|s| overlaps(s, output_span)))
        || span(binding.entry.original, binding.entry.trampoline.len())
            .is_none_or(|s| overlaps(s, output_span))
    {
        return Err("output aliases kernel code");
    }
    if binding
        .matched
        .constants
        .iter()
        .any(|c| span(c.address, c.bytes.len()).is_none_or(|s| overlaps(s, output_span)))
    {
        return Err("output aliases kernel constants");
    }
    let mut address = output;
    while address < output_span.1 {
        let len = (4096 - (address & 4095)).min(output_span.1 - address);
        if e.cpu.accessible(address, len, Prot::EXEC) {
            return Err("executable output buffer");
        }
        address += len;
    }
    let mut trig_bytes = vec![0; trig_len];
    let mut bit_bytes = vec![0; bit_len];
    e.cpu
        .read(trig, &mut trig_bytes)
        .map_err(|_| "unreadable lookup tables")?;
    e.cpu
        .read(bitrev, &mut bit_bytes)
        .map_err(|_| "unreadable lookup tables")?;
    let slot = n.ilog2() as usize - 6;
    let registers = e.ext.get::<Provider>().bindings[index]
        .matched
        .profile
        .kernel
        == REGISTER_KERNEL;
    let provider = e.ext.get_mut::<Provider>();
    let tables = provider.tables[usize::from(registers)][slot]
        .get_or_insert_with(|| StandardTables::new(n, registers));
    let trig_mismatch = trig_bytes
        .as_chunks::<4>()
        .0
        .iter()
        .zip(&tables.trig)
        .enumerate()
        .find_map(|(index, (bytes, &expected))| {
            let actual = f32::from_le_bytes(*bytes);
            (!ulp_match(actual, expected)).then_some((
                "trig",
                index,
                actual.to_bits(),
                expected.to_bits(),
            ))
        });
    let bit_mismatch = bit_bytes
        .as_chunks::<4>()
        .0
        .iter()
        .zip(&tables.bitrev)
        .enumerate()
        .find_map(|(index, (bytes, &expected))| {
            let actual = i32::from_le_bytes(*bytes);
            (actual != expected).then_some(("bitrev", index, actual as u32, expected as u32))
        });
    if let Some((table, index, actual, expected)) = trig_mismatch.or(bit_mismatch) {
        provider.stats.table_mismatch.get_or_insert_with(|| {
            format!("n={n} table={table} index={index} actual={actual:08x} expected={expected:08x}")
        });
        return Err("nonstandard lookup tables");
    }
    let mut bytes = vec![0; n * 2];
    e.cpu
        .read(source, &mut bytes)
        .map_err(|_| "unreadable spectrum")?;
    let values: Vec<_> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    if values
        .iter()
        .any(|v| !v.is_finite() || (v.to_bits() & 0x7fff_ffff != 0 && !v.is_normal()))
    {
        return Err("unsupported spectrum value");
    }
    Ok((output, values))
}

fn compare(provider: &mut Provider, candidate: &[f32], bytes: &[u8]) {
    for (&native, reference) in candidate.iter().zip(bytes.as_chunks::<4>().0) {
        let guest = f32::from_le_bytes(*reference);
        provider.stats.compared_floats += 1;
        provider.stats.different_floats += u64::from(native.to_bits() != guest.to_bits());
        if !guest.is_finite() {
            provider.stats.nonfinite_floats += 1;
            continue;
        }
        let error = f64::from(native) - f64::from(guest);
        provider.stats.max_abs = provider.stats.max_abs.max(error.abs());
        provider.squared_error += error * error;
        provider.squared_reference += f64::from(guest) * f64::from(guest);
    }
    let count = provider.stats.compared_floats - provider.stats.nonfinite_floats;
    provider.stats.rms = (provider.squared_error / count.max(1) as f64).sqrt();
    provider.stats.relative_rms = if provider.squared_reference > 0.0 {
        (provider.squared_error / provider.squared_reference).sqrt()
    } else if provider.squared_error == 0.0 {
        0.0
    } else {
        f64::MAX
    };
}

fn candidate(e: &mut Env<'_>, index: usize) -> Result<(u32, Vec<f32>, u32), &'static str> {
    e.cpu.quiesce_memory();
    let binding = &e.ext.get::<Provider>().bindings[index];
    if !binding.live {
        return Err("binding image retired");
    }
    if !identical(e.cpu, binding.entry.original, &binding.entry.trampoline)
        || !e.cpu.accessible(
            binding.entry.original,
            binding.entry.trampoline.len() as u32,
            Prot::EXEC,
        )
    {
        return Err("original trampoline changed");
    }
    if binding.revoked {
        return Err("binding revoked");
    }
    if !closure_intact(e.cpu, binding) {
        e.sys.redirects.remove(&binding.entry.entry);
        let provider = e.ext.get_mut::<Provider>();
        provider.bindings[index].revoked = true;
        provider.stats.revocations += 1;
        return Err("kernel code or constants changed");
    }
    let architecture = match e.cpu.capture_architecture() {
        Ok(state) => state,
        Err(_) => return Err("unsupported architectural capture"),
    };
    let registers = e.ext.get::<Provider>().bindings[index]
        .matched
        .profile
        .kernel
        == REGISTER_KERNEL;
    let floating = if registers {
        architecture.mxcsr & 0xffc0 == 0x1f80
    } else {
        architecture.x87.tag == 0xffff
            && architecture.x87.status & 0x80 == 0
            && architecture.x87.control & 0x3f == 0x3f
            && architecture.x87.control & 0x0c00 == 0
            && matches!(architecture.x87.control & 0x0300, 0x0200 | 0x0300)
    };
    if architecture.eflags & (1 << 8) != 0 || !floating {
        return Err("unsupported floating environment");
    }
    let (output, values) = input(e, index)?;
    let started = Instant::now();
    let candidate = e
        .ext
        .get_mut::<Provider>()
        .cache
        .inverse(&values)
        .map(<[f32]>::to_vec);
    let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
    let provider = e.ext.get_mut::<Provider>();
    provider.stats.total_kernel_ns = provider.stats.total_kernel_ns.saturating_add(elapsed);
    provider.stats.max_kernel_ns = provider.stats.max_kernel_ns.max(elapsed);
    let candidate = match candidate {
        Ok(candidate) => candidate,
        Err(_) => return Err("native kernel rejected spectrum"),
    };
    Ok((output, candidate, architecture.regs[0]))
}

// spec: CODEC-CALL-1, DEV-AUDIO-4
fn native(e: &mut Env<'_>) -> Option<u64> {
    if e.ext.get::<Provider>().stats.mode != Mode::Native {
        return None;
    }
    let index = e.sys.thunks.get(e.thunk).f.data as usize;
    let (output, candidate, eax) = match candidate(e, index) {
        Ok(value) => value,
        Err(reason) => {
            e.ext.get_mut::<Provider>().rejection(reason);
            return None;
        }
    };
    let bytes: Vec<_> = candidate.iter().flat_map(|v| v.to_le_bytes()).collect();
    // input() checked ordinary writable storage before computing the candidate.
    e.cpu
        .write(output, &bytes)
        .expect("admitted codec output storage");
    let stats = &mut e.ext.get_mut::<Provider>().stats;
    stats.calls += 1;
    stats.native_calls += 1;
    Some(u64::from(eax))
}

fn call(e: &mut Env<'_>) -> Outcome {
    let index = e.sys.thunks.get(e.thunk).f.data as usize;
    if e.ext.get::<Provider>().stats.mode == Mode::Native {
        if let Some(value) = native(e) {
            return Outcome::Return(value as u32);
        }
        let stats = &mut e.ext.get_mut::<Provider>().stats;
        stats.calls += 1;
        stats.fallback_calls += 1;
        return original(e, index);
    }
    let (output, candidate, _) = match candidate(e, index) {
        Ok(value) => value,
        Err(reason) => {
            let provider = e.ext.get_mut::<Provider>();
            provider.stats.fallback_calls += 1;
            provider.rejection(reason);
            return original(e, index);
        }
    };
    let provider = e.ext.get_mut::<Provider>();
    provider.stats.calls += 1;
    provider.stats.verify_calls += 1;
    let target = provider.bindings[index].entry.original;
    let registers = provider.bindings[index].matched.profile.kernel == REGISTER_KERNEL;
    let Ok([lookup, source, _]) = arguments(e, index) else {
        return original(e, index);
    };
    let args = if registers {
        vec![output]
    } else {
        vec![lookup, source, output]
    };
    Outcome::call(target, args, move |e, resume| {
        let mut bytes = vec![0; candidate.len() * 4];
        if e.cpu.read(output, &mut bytes).is_ok() {
            compare(e.ext.get_mut::<Provider>(), &candidate, &bytes);
        } else {
            let provider = e.ext.get_mut::<Provider>();
            provider.stats.errors += 1;
            provider.rejection("verification output unavailable");
        }
        Outcome::Return(resume.value())
    })
}

#[cfg(test)]
#[path = "../../tests/codec/mdct.rs"]
mod tests;
