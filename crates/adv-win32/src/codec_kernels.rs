//! Explicit library-ABI acceleration for verified static codec kernels.
//!
//! Guest owns all stream, seek, cancellation and allocation state. Native mode
//! replaces only an admitted inverse MDCT and permits decoder rounding differences;
//! it is not an instruction-exact x87 implementation. Verify always publishes the
//! original guest computation, never promotes a binding, and never writes its candidate.
use std::collections::BTreeMap;
use std::time::Instant;

use adv_abi::Registry;
use adv_abi::code_signature::{self, ExecutableRange, Match, Signature};
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot, Reg};
use adv_device::codec_kernel::ImdctCache;
use adv_machine::layout::thunk_addr;
use adv_machine::patch::Detour;
use adv_machine::{Conv, Env, Machine, Outcome};
use adv_os::{ModuleId, ModuleKind};

const MAX_BINDINGS: usize = 64;
const MAX_CODE_BYTES: usize = 16 * 1024;
const KERNEL: &str = "vorbis-mdct-backward-f32-cdecl32";
const BINDING_MODULE: &str = "adv-codec-kernel";
// Reviewed library-build contract. The descriptor must state this explicitly;
// double-PI or other builds require their own reviewed numerical profile.
const TABLE_PI_BITS: u32 = 0x4049_0fdb;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Guest,
    Verify,
    Native,
}
impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Guest => "guest",
            Self::Verify => "verify",
            Self::Native => "native",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct InstallReport {
    pub mode: Mode,
    pub candidates: u64,
    pub installed: u64,
    pub rejected: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
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
    module: ModuleId,
    module_base: u32,
    module_size: u32,
    matched: Match,
    detour: Detour,
    saved_prefix: Vec<u8>,
    installed_prefix: Vec<u8>,
    trampoline: Vec<u8>,
    revoked: bool,
}

struct StandardTables {
    trig: Vec<f32>,
    bitrev: Vec<i32>,
}
impl StandardTables {
    fn new(n: usize, pi_bits: u32) -> Self {
        let mut trig = vec![0.0; n + n / 4];
        let pi = f64::from(f32::from_bits(pi_bits));
        for i in 0..n / 4 {
            let a = (pi / n as f64) * (4 * i) as f64;
            trig[2 * i] = a.cos() as f32;
            trig[2 * i + 1] = (-a.sin()) as f32;
            let b = (pi / (2 * n) as f64) * (2 * i + 1) as f64;
            trig[n / 2 + 2 * i] = b.cos() as f32;
            trig[n / 2 + 2 * i + 1] = b.sin() as f32;
        }
        for i in 0..n / 8 {
            let a = (pi / n as f64) * (4 * i + 2) as f64;
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
    tables: [Option<StandardTables>; 8],
    stats: Stats,
    squared_error: f64,
    squared_reference: f64,
}
impl Provider {
    fn new(mode: Mode) -> Self {
        Self {
            bindings: Vec::new(),
            cache: ImdctCache::default(),
            tables: std::array::from_fn(|_| None),
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

pub fn stats<B: Backend>(machine: &Machine<B>) -> Option<Stats> {
    machine.ext.try_get::<Provider>().map(|p| p.stats.clone())
}

/// Install only after guest images are mapped, before executing any guest code.
/// The caller supplies an explicit external signature database. There is no
/// embedded default database and Guest does not parse or patch anything.
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    mode: Mode,
    signature_text: &str,
) -> Result<InstallReport, String> {
    let mut report = InstallReport {
        mode,
        ..InstallReport::default()
    };
    if mode == Mode::Guest {
        return Ok(report);
    }
    if machine.sys.host_calls != 0 {
        return Err("codec kernels must be installed before guest process execution".into());
    }
    if machine.ext.try_get::<Provider>().is_some() {
        return Err("codec kernel provider already installed".into());
    }
    let signature = Signature::parse(signature_text).map_err(|e| e.to_string())?;
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
    if signature.profile.kernel != KERNEL
        || required
            .iter()
            .any(|&(key, value)| properties.get(key) != Some(&value))
    {
        return Err("unsupported codec kernel ABI profile".into());
    }
    machine.cpu.quiesce_memory();
    machine
        .cpu
        .capture_architecture()
        .map_err(|e| e.to_string())?;
    let images: Vec<_> = adv_os::try_modules(&machine.ext)
        .ok_or("codec kernels require mapped guest modules")?
        .iter()
        .filter(|(_, m)| m.kind == ModuleKind::Image && m.lifetime.mapped)
        .map(|(id, m)| (id, m.base, m.size))
        .collect();
    let mut candidates = Vec::new();
    for (module, base, size) in images {
        let mut header = vec![0; (size as usize).min(65_536)];
        if machine.cpu.read(base, &mut header).is_err() {
            continue;
        }
        let Ok(pe) = adv_pe::Pe::parse_headers(&header) else {
            continue;
        };
        let ranges: Vec<_> = pe
            .sections
            .iter()
            .filter_map(|section| {
                if section.characteristics & adv_pe::scn::MEM_EXECUTE == 0 {
                    return None;
                }
                let start = base.checked_add(section.virtual_address)?;
                let len = section
                    .virtual_size
                    .max(section.raw_size)
                    .min(size.checked_sub(section.virtual_address)?);
                (len != 0).then_some(ExecutableRange { start, len })
            })
            .collect();
        let found = match code_signature::scan(&signature, &ranges, &mut |address, len| {
            let mut bytes = vec![0; len];
            machine
                .cpu
                .read(address, &mut bytes)
                .map_err(|e| e.to_string())?;
            Ok(bytes)
        }) {
            Ok(found) => found,
            Err(code_signature::Error::NotFound) => continue,
            Err(error) => {
                report.rejected += 1;
                report.reason.get_or_insert_with(|| error.to_string());
                continue;
            }
        };
        report.candidates += found.len() as u64;
        for matched in found {
            let code_bytes: usize = matched.functions.iter().map(|f| f.bytes.len()).sum();
            let complete = matched.functions.iter().all(|f| {
                let Some(offset) = f.entry.checked_sub(base) else {
                    return false;
                };
                offset
                    .checked_add(f.bytes.len() as u32)
                    .is_some_and(|end| end <= size)
            });
            if matched.profile.kernel != KERNEL
                || !(5..=32).contains(&matched.prologue_len)
                || code_bytes > MAX_CODE_BYTES
                || !complete
                || candidates.len() == MAX_BINDINGS
            {
                report.rejected += 1;
                continue;
            }
            candidates.push((module, base, size, matched));
        }
    }
    machine.ext.insert(Provider::new(mode));
    for (module, module_base, module_size, matched) in candidates {
        let index = machine.ext.get::<Provider>().bindings.len();
        let name = format!("mdct_backward_{index}");
        machine
            .sys
            .thunks
            .resolver_mut::<Registry>()
            .ok_or("codec kernels require the ABI registry resolver")?
            .add_with_data(
                BINDING_MODULE,
                &name,
                Conv::Cdecl { argc: 3 },
                call,
                index as u64,
            );
        let p = matched.prologue_len;
        let root = matched
            .functions
            .iter()
            .find(|f| f.entry == matched.entry)
            .ok_or("signature match has no root function")?;
        let saved_prefix = root
            .bytes
            .get(..p)
            .ok_or("signature prologue exceeds root function")?
            .to_vec();
        let detour = machine
            .detour(matched.entry, p as u32, BINDING_MODULE, &name)
            .map_err(|e| e.to_string())?;
        let mut installed = Asm::new(detour.entry);
        installed.jmp(thunk_addr(detour.thunk));
        installed.bytes.resize(p, 0xcc);
        let mut trampoline = Asm::new(detour.original);
        trampoline.bytes.extend_from_slice(&saved_prefix);
        trampoline.jmp(detour.entry + p as u32);
        let provider = machine.ext.get_mut::<Provider>();
        provider.bindings.push(Binding {
            module,
            module_base,
            module_size,
            matched,
            detour,
            saved_prefix,
            installed_prefix: installed.bytes,
            trampoline: trampoline.bytes,
            revoked: false,
        });
        provider.stats.installed += 1;
        report.installed += 1;
    }
    if report.installed == 0 {
        let reason = "no verified static codec kernel matched the supplied signatures";
        report.reason = Some(reason.to_owned());
        machine.ext.get_mut::<Provider>().stats.last_rejection = report.reason.clone();
    }
    Ok(report)
}

fn identical(cpu: &dyn Guest, address: u32, expected: &[u8]) -> bool {
    let mut actual = vec![0; expected.len()];
    cpu.read(address, &mut actual).is_ok() && actual == expected
}

fn module_live(e: &Env<'_>, binding: &Binding) -> bool {
    adv_os::try_modules(e.ext)
        .and_then(|m| m.try_get(binding.module))
        .is_some_and(|m| {
            m.kind == ModuleKind::Image
                && m.lifetime.mapped
                && m.base == binding.module_base
                && m.size == binding.module_size
        })
}

/// A captured old jump can still invoke this thunk after revocation. Always
/// continue the independent saved-prologue trampoline, never the current entry.
fn original(e: &mut Env<'_>, index: usize) -> Outcome {
    let binding = &e.ext.get::<Provider>().bindings[index];
    if !module_live(e, binding) || !identical(e.cpu, binding.detour.original, &binding.trampoline) {
        e.ext.get_mut::<Provider>().stats.errors += 1;
        return Outcome::Fail("codec kernel original continuation is no longer verified".into());
    }
    let target = binding.detour.original;
    match e.checkpoint() {
        Ok(mut checkpoint) => {
            checkpoint.state.eip = target;
            Outcome::ResumeExecution(checkpoint)
        }
        Err(error) => Outcome::Fail(format!("codec kernel original checkpoint: {error}")),
    }
}

fn fallback(e: &mut Env<'_>, index: usize, reason: &'static str) -> Outcome {
    let provider = e.ext.get_mut::<Provider>();
    provider.stats.fallback_calls += 1;
    provider.rejection(reason);
    original(e, index)
}

fn closure_intact(cpu: &dyn Guest, binding: &Binding) -> bool {
    binding.matched.functions.iter().all(|f| {
        if !cpu.accessible(f.entry, f.bytes.len() as u32, Prot::EXEC) {
            return false;
        }
        if f.entry == binding.detour.entry {
            let p = binding.saved_prefix.len();
            identical(cpu, f.entry + p as u32, &f.bytes[p..])
        } else {
            identical(cpu, f.entry, &f.bytes)
        }
    }) && binding.matched.constants.iter().all(|c| {
        cpu.accessible(c.address, c.bytes.len() as u32, Prot::READ)
            && identical(cpu, c.address, &c.bytes)
    })
}

fn revoke(e: &mut Env<'_>, index: usize) -> Result<(), &'static str> {
    let binding = &e.ext.get::<Provider>().bindings[index];
    let owned = identical(e.cpu, binding.detour.entry, &binding.installed_prefix);
    // If only our trailing padding changed, restoring the prefix would erase a
    // foreign patch; running the old displaced instructions would also guess its intent.
    if !owned && identical(e.cpu, binding.detour.entry, &binding.installed_prefix[..5]) {
        return Err("codec kernel displaced prologue changed ambiguously");
    }
    if owned
        && e.cpu
            .write(binding.detour.entry, &binding.saved_prefix)
            .is_err()
    {
        return Err("codec kernel revocation could not restore its own entry");
    }
    let provider = e.ext.get_mut::<Provider>();
    provider.bindings[index].revoked = true;
    provider.stats.revocations += 1;
    Ok(())
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

/// Read fresh table contents on every call. A heap pointer is never a content identity.
fn input(e: &mut Env<'_>, index: usize) -> Result<(u32, Vec<f32>), &'static str> {
    let lookup = e.cpu.arg(0).map_err(|_| "unreadable arguments")?;
    let source = e.cpu.arg(1).map_err(|_| "unreadable arguments")?;
    let output = e.cpu.arg(2).map_err(|_| "unreadable arguments")?;
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
        || span(binding.detour.original, binding.trampoline.len())
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
    let pi_bits = e.ext.get::<Provider>().bindings[index]
        .matched
        .profile
        .properties
        .get("table_pi_bits")
        .copied()
        .ok_or("unsupported table numerical profile")?;
    if pi_bits != TABLE_PI_BITS {
        return Err("unsupported table numerical profile");
    }
    let provider = e.ext.get_mut::<Provider>();
    let tables = provider.tables[slot].get_or_insert_with(|| StandardTables::new(n, pi_bits));
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

fn call(e: &mut Env<'_>) -> Outcome {
    let index = e.sys.thunks.get(e.thunk).f.data as usize;
    let Some(provider) = e.ext.try_get_mut::<Provider>() else {
        return Outcome::Fail("codec kernel provider missing".into());
    };
    provider.stats.calls += 1;
    if index >= provider.bindings.len() {
        return Outcome::Fail("codec kernel binding missing".into());
    }
    e.cpu.quiesce_memory();
    let binding = &e.ext.get::<Provider>().bindings[index];
    if !module_live(e, binding) {
        e.ext.get_mut::<Provider>().stats.errors += 1;
        return Outcome::Fail("codec kernel binding image was retired".into());
    }
    if !identical(e.cpu, binding.detour.original, &binding.trampoline) {
        e.ext.get_mut::<Provider>().stats.errors += 1;
        return Outcome::Fail("codec kernel original trampoline was modified".into());
    }
    if !e.cpu.accessible(
        binding.detour.original,
        binding.trampoline.len() as u32,
        Prot::EXEC,
    ) {
        return fallback(e, index, "original trampoline not executable");
    }
    if binding.revoked {
        return fallback(e, index, "binding revoked");
    }
    if adv_os::modules(e.ext)
        .get(binding.module)
        .lifetime
        .unloading
    {
        if let Err(error) = revoke(e, index) {
            e.ext.get_mut::<Provider>().stats.errors += 1;
            return Outcome::Fail(error.into());
        }
        return fallback(e, index, "binding image unloading");
    }
    if !identical(e.cpu, binding.detour.entry, &binding.installed_prefix)
        || !closure_intact(e.cpu, binding)
    {
        if let Err(error) = revoke(e, index) {
            e.ext.get_mut::<Provider>().stats.errors += 1;
            return Outcome::Fail(error.into());
        }
        return fallback(e, index, "kernel code or constants changed");
    }
    let architecture = match e.cpu.capture_architecture() {
        Ok(state) => state,
        Err(_) => return fallback(e, index, "unsupported architectural capture"),
    };
    if architecture.eflags & (1 << 8) != 0
        || architecture.x87.tag != 0xffff
        || architecture.x87.status & 0x80 != 0
        || architecture.x87.control & 0x3f != 0x3f
        || architecture.x87.control & 0x0c00 != 0
        || !matches!(architecture.x87.control & 0x0300, 0x0200 | 0x0300)
    {
        return fallback(e, index, "unsupported floating environment");
    }
    let (output, values) = match input(e, index) {
        Ok(value) => value,
        Err(reason) => return fallback(e, index, reason),
    };
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
        Err(_) => return fallback(e, index, "native kernel rejected spectrum"),
    };
    if provider.stats.mode == Mode::Verify {
        provider.stats.verify_calls += 1;
        let target = provider.bindings[index].detour.original;
        let args = [e.cpu.arg(0), e.cpu.arg(1), e.cpu.arg(2)];
        let [Ok(lookup), Ok(source), Ok(output)] = args else {
            return fallback(e, index, "unreadable arguments");
        };
        return Outcome::call(target, [lookup, source, output], move |e, resume| {
            let mut bytes = vec![0; candidate.len() * 4];
            if e.cpu.read(output, &mut bytes).is_ok() {
                compare(e.ext.get_mut::<Provider>(), &candidate, &bytes);
            } else {
                let provider = e.ext.get_mut::<Provider>();
                provider.stats.errors += 1;
                provider.rejection("verification output unavailable");
            }
            // Preserve the actual guest result and floating environment. In
            // particular, never restore the pre-original x87 checkpoint here.
            Outcome::Return(resume.value())
        });
    }
    let bytes: Vec<_> = candidate.iter().flat_map(|v| v.to_le_bytes()).collect();
    if e.cpu.write(output, &bytes).is_err() {
        // A failing backend write may already have changed an in-place input.
        // Running original after a partially committed write would be incorrect.
        e.ext.get_mut::<Provider>().stats.errors += 1;
        return Outcome::Fail("codec kernel output commit failed".into());
    }
    e.ext.get_mut::<Provider>().stats.native_calls += 1;
    Outcome::Return(architecture.regs[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_abi::code_signature::{FunctionSnapshot, Profile};
    use adv_cpu::JitCpu;
    use adv_machine::layout::{PAGE, PARK_ADDR};
    use adv_machine::{Clock, RunResult};
    use adv_os::module::{Lifetime, Module, Modules};

    const CODE: u32 = 0x0040_0000;
    const DATA: u32 = 0x0041_0000;
    const ENTRY: u32 = CODE + 0x200;
    const LOOKUP: u32 = DATA + 0x100;
    const TRIG: u32 = DATA + 0x1000;
    const BITREV: u32 = DATA + 0x2000;
    const INPUT: u32 = DATA + 0x3000;
    const OUTPUT: u32 = DATA + 0x4000;
    const RESULT: u32 = 0xabcd_1234;

    /// An authored ABI fixture, not a codec fingerprint. It writes one sample,
    /// raises a masked x87 divide-by-zero, and returns a diagnostic EAX value.
    fn fixture(mode: Mode) -> Machine<JitCpu> {
        let mut registry = Registry::default();
        registry.add_with_data(BINDING_MODULE, "fixture", Conv::Cdecl { argc: 3 }, call, 0);
        registry.add("test", "done", Conv::Cdecl { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(CODE, PAGE, Prot::RX).unwrap();
        m.cpu.map(DATA, PAGE * 8, Prot::RW).unwrap();
        let mut body = Asm::new(ENTRY);
        body.bytes.extend_from_slice(&[
            0x83, 0xec, 0x20, // sub esp,32
            0x53, // push ebx
            0x8b, 0x5c, 0x24, 0x28, // mov ebx,[esp+40]
            0x8b, 0x4c, 0x24, 0x30, // mov ecx,[esp+48] (output)
            0xc7, 0x01, // mov dword [ecx],42.0f
        ]);
        body.bytes.extend_from_slice(&42.0f32.to_le_bytes());
        body.bytes.extend_from_slice(&[
            0xd9, 0xe8, // fld1
            0xd9, 0xee, // fldz
            0xde, 0xf9, // fdivp st(1),st(0)
            0xdd, 0xd8, // fstp st(0)
            0xb8, // mov eax,RESULT
        ]);
        body.bytes.extend_from_slice(&RESULT.to_le_bytes());
        body.bytes.push(0x5b); // pop ebx
        body.add_esp(32).ret();
        m.cpu.write(ENTRY, &body.bytes).unwrap();
        let mut modules = Modules::default();
        let module = modules.add(Module {
            name: "fixture.exe".into(),
            path: "fixture.exe".into(),
            kind: ModuleKind::Image,
            base: CODE,
            size: PAGE,
            is_dll: false,
            entry: CODE,
            stack_reserve: 0x10000,
            exports_by_name: Default::default(),
            exports_by_ordinal: Default::default(),
            resources: None,
            data_file: None,
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime: Lifetime {
                mapped: true,
                ..Lifetime::default()
            },
        });
        m.ext.insert(modules);
        let matched = Match {
            entry: ENTRY,
            prologue_len: 8,
            profile: Profile {
                kernel: KERNEL.into(),
                properties: BTreeMap::from([("table_pi_bits".into(), TABLE_PI_BITS)]),
            },
            functions: vec![FunctionSnapshot {
                role: "main".into(),
                entry: ENTRY,
                bytes: body.bytes.clone(),
            }],
            constants: Vec::new(),
        };
        let saved_prefix = body.bytes[..8].to_vec();
        let detour = m.detour(ENTRY, 8, BINDING_MODULE, "fixture").unwrap();
        let mut prefix = Asm::new(ENTRY);
        prefix.jmp(thunk_addr(detour.thunk));
        prefix.bytes.resize(8, 0xcc);
        let mut trampoline = Asm::new(detour.original);
        trampoline.bytes.extend_from_slice(&saved_prefix);
        trampoline.jmp(ENTRY + 8);
        let mut provider = Provider::new(mode);
        provider.stats.installed = 1;
        provider.bindings.push(Binding {
            module,
            module_base: CODE,
            module_size: PAGE,
            matched,
            detour,
            saved_prefix,
            installed_prefix: prefix.bytes,
            trampoline: trampoline.bytes,
            revoked: false,
        });
        m.ext.insert(provider);
        let tables = StandardTables::new(64, TABLE_PI_BITS);
        let trig: Vec<_> = tables.trig.iter().flat_map(|f| f.to_le_bytes()).collect();
        let bitrev: Vec<_> = tables.bitrev.iter().flat_map(|i| i.to_le_bytes()).collect();
        m.cpu.write(TRIG, &trig).unwrap();
        m.cpu.write(BITREV, &bitrev).unwrap();
        for (offset, value) in [
            (0, 64),
            (4, 6),
            (8, TRIG),
            (12, BITREV),
            (16, (4.0f32 / 64.0).to_bits()),
        ] {
            m.cpu.write_u32(LOOKUP + offset, value).unwrap();
        }
        let done = m.resolve_import("test", "done").unwrap();
        let mut caller = Asm::new(CODE);
        caller
            .push_imm(OUTPUT)
            .push_imm(INPUT)
            .push_imm(LOOKUP)
            .call(ENTRY)
            .add_esp(12)
            .mov_mem_eax(DATA)
            .call(done);
        m.cpu.write(CODE, &caller.bytes).unwrap();
        m.spawn("codec ABI fixture", CODE, &[], 0x10000).unwrap();
        m
    }

    #[test]
    fn verify_keeps_original_output_eax_and_floating_exception_state() {
        let mut m = fixture(Mode::Verify);
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), RESULT);
        assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 42.0f32.to_bits());
        let architecture = m.cpu.capture_architecture().unwrap();
        assert_eq!(architecture.x87.control, 0x027f);
        assert_eq!(architecture.x87.tag, 0xffff);
        assert_ne!(architecture.x87.status & 4, 0);
        let stats = stats(&m).unwrap();
        assert_eq!(
            (stats.native_calls, stats.verify_calls, stats.fallback_calls),
            (0, 1, 0)
        );
        assert_eq!(stats.compared_floats, 64);
        // FFT output also contains negative zeros: the diagnostic deliberately
        // counts bit differences even when their absolute numeric error is zero.
        assert!(stats.different_floats >= 1);
        assert_eq!(stats.max_abs, 42.0);
    }

    #[test]
    fn foreign_hook_chaining_old_jump_revokes_and_continues_original_without_recursion() {
        let mut m = fixture(Mode::Native);
        let thunk = m.ext.get::<Provider>().bindings[0].detour.thunk;
        let mut hook = Asm::new(CODE + 0x700);
        hook.jmp(thunk_addr(thunk));
        m.cpu.write(CODE + 0x700, &hook.bytes).unwrap();
        let mut entry = Asm::new(ENTRY);
        entry.jmp(CODE + 0x700);
        m.cpu.write(ENTRY, &entry.bytes).unwrap();
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), RESULT);
        assert!(identical(&m.cpu, ENTRY, &entry.bytes));
        let stats = stats(&m).unwrap();
        assert_eq!(
            (stats.native_calls, stats.fallback_calls, stats.revocations),
            (0, 1, 1)
        );
    }

    #[test]
    fn modified_original_trampoline_fails_before_following_a_recursive_old_jump() {
        let mut m = fixture(Mode::Native);
        let binding = &m.ext.get::<Provider>().bindings[0];
        let mut recursive = Asm::new(binding.detour.original);
        recursive.jmp(thunk_addr(binding.detour.thunk));
        m.cpu
            .write(binding.detour.original, &recursive.bytes)
            .unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(stats(&m).unwrap().errors, 1);
        assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 0);
    }

    #[test]
    fn changed_lookup_contents_fall_back_before_publishing_native_output() {
        let mut m = fixture(Mode::Native);
        m.cpu.write_u32(BITREV, 29).unwrap(); // standard first index is 30
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 42.0f32.to_bits());
        let stats = stats(&m).unwrap();
        assert_eq!((stats.native_calls, stats.fallback_calls), (0, 1));
        assert_eq!(
            stats.last_rejection.as_deref(),
            Some("nonstandard lookup tables")
        );
    }

    #[test]
    fn reviewed_f32_pi_profile_has_the_initializers_table_values() {
        let tables = StandardTables::new(256, TABLE_PI_BITS);
        assert_eq!(tables.trig[0].to_bits(), 1.0f32.to_bits());
        assert_eq!(tables.trig[1].to_bits(), (-0.0f32).to_bits());
        // Independently observed standard initializer value. Using full f64 PI
        // gives 0x3e78cfcc and incorrectly rejects this verified f32 build.
        assert_eq!(tables.trig[54].to_bits(), 0x3e78_cfc9);
        assert_eq!(&tables.bitrev[..4], &[126, 0, 62, 64]);
        assert!(!ulp_match(
            f32::from_bits(0x3e78_cfc9),
            f32::from_bits(0x3e78_cfcc)
        ));
    }
}
