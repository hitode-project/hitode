//! Static requirement survey of a game's modules (PERS-EXPORT-9). All of a
//! game's native code is on disk, so before it runs we can list what it can
//! reach: static and delay imports, and the modules, functions and COM classes
//! it names by constant at LoadLibrary, GetProcAddress and CoCreateInstance
//! call sites. Diffing them against the runtime lists every gap in one run,
//! not just the first one a player reaches.
use std::collections::{BTreeMap, BTreeSet, HashMap};

use adv_abi::Registry;
use adv_abi::personality::ExportClass;
use adv_cpu::backend::Backend;
use adv_machine::Resolver;
use adv_pe::{ImportName, Pe};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

use crate::boot::Runtime;

/// What one module requires or provides, by name only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Static and delay imports as (lowercase module, name or `#ordinal`).
    pub imports: BTreeSet<(String, String)>,
    /// Export names and `#ordinal`s, for imports from other game modules.
    pub exports: BTreeSet<String>,
    /// Modules loaded or queried by constant name, as lowercase file names.
    pub loads: BTreeSet<String>,
    /// Functions looked up by constant name or `#ordinal`.
    pub lookups: BTreeSet<String>,
    /// COM classes created by constant CLSID.
    pub classes: BTreeSet<[u8; 16]>,
    /// Calls to the load, lookup and class functions above.
    pub sites: u32,
    /// Of those, the calls whose name or class argument is not a constant.
    pub open_sites: u32,
}

/// A game file, by its path under the game folder, and its manifest.
pub struct Module {
    pub file: String,
    pub manifest: Manifest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lookup {
    Load {
        wide: bool,
    },
    /// GetModuleHandle: NULL for a module not loaded is an ordinary answer.
    Query {
        arg: usize,
    },
    Proc,
    Class,
}

fn lookup(name: &str) -> Option<Lookup> {
    Some(match name {
        "LoadLibraryA" | "LoadLibraryExA" => Lookup::Load { wide: false },
        "LoadLibraryW" | "LoadLibraryExW" => Lookup::Load { wide: true },
        "GetModuleHandleA" | "GetModuleHandleW" => Lookup::Query { arg: 0 },
        "GetModuleHandleExA" | "GetModuleHandleExW" => Lookup::Query { arg: 1 },
        "GetProcAddress" => Lookup::Proc,
        "CoCreateInstance" | "CoCreateInstanceEx" | "CoGetClassObject" => Lookup::Class,
        _ => return None,
    })
}

/// The manifest of a native x86 PE file, or None when the file is not one.
pub fn analyze(file: &[u8]) -> Option<Manifest> {
    let pe = Pe::parse(file).ok()?;
    // A CLR header marks a managed assembly, which the runtime does not run.
    if pe.machine != 0x14c || pe.dir(14).is_present() {
        return None;
    }
    let mut manifest = Manifest::default();
    let mut slots = HashMap::new();
    let imports = pe
        .imports()
        .unwrap_or_default()
        .into_iter()
        .map(|module| (module.dll, module.imports))
        .chain(
            pe.delay_imports()
                .unwrap_or_default()
                .into_iter()
                .map(|module| (module.dll, module.imports)),
        );
    for (dll, imports) in imports {
        let dll = dll.to_ascii_lowercase();
        for import in imports {
            if let ImportName::Name { name, .. } = &import.name
                && let Some(kind) = lookup(name)
            {
                slots.insert(pe.image_base.wrapping_add(import.iat_rva), kind);
            }
            manifest
                .imports
                .insert((dll.clone(), import.name.to_string()));
        }
    }
    for export in pe.exports().unwrap_or_default() {
        manifest.exports.insert(format!("#{}", export.ordinal));
        manifest.exports.extend(export.name);
    }
    if !slots.is_empty() {
        let image = Image {
            base: pe.image_base,
            bytes: &pe.image,
        };
        for section in pe.sections.iter().filter(|s| s.is_executable()) {
            let start = section.virtual_address as usize;
            let end = start
                .saturating_add(section.mem_size() as usize)
                .min(pe.image.len());
            if let Some(code) = pe.image.get(start..end) {
                let ip = pe.image_base.wrapping_add(section.virtual_address);
                scan(code, ip, &slots, &image, &mut manifest);
            }
        }
    }
    Some(manifest)
}

/// The mapped image, addressed by its preferred-base virtual addresses.
struct Image<'a> {
    base: u32,
    bytes: &'a [u8],
}

impl Image<'_> {
    fn at(&self, va: u32) -> &[u8] {
        va.checked_sub(self.base)
            .and_then(|rva| self.bytes.get(rva as usize..))
            .unwrap_or(&[])
    }

    /// A terminated printable ASCII name of at most MAX_PATH characters.
    fn name(&self, va: u32, wide: bool) -> Option<String> {
        let bytes = self.at(va);
        let text = if wide {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&unit| u16::from_le_bytes(unit))
                .take(261)
                .take_while(|&unit| unit != 0)
                .collect();
            if units.len() > 260 {
                return None;
            }
            String::from_utf16(&units).ok()?
        } else {
            let end = bytes.iter().take(261).position(|&b| b == 0)?;
            String::from_utf8(bytes[..end].to_vec()).ok()?
        };
        (!text.is_empty() && text.chars().all(|c| c.is_ascii_graphic() || c == ' ')).then_some(text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    Const(u32),
    /// The function pointer read from an import slot.
    Slot(u32),
}

/// Values known since the last control-flow boundary of a linear sweep.
#[derive(Default)]
struct State {
    regs: [Option<Value>; 8],
    pushes: Vec<Option<Value>>,
    /// `mov [esp+n], value` stores, for compilers that do not push arguments.
    stores: HashMap<u32, Option<Value>>,
}

impl State {
    /// Argument `index` of a call made now.
    fn arg(&self, index: usize) -> Option<Value> {
        if self.pushes.is_empty() {
            return self.stores.get(&(4 * index as u32)).copied().flatten();
        }
        let at = self.pushes.len().checked_sub(index + 1)?;
        self.pushes[at]
    }

    /// A callee consumes its arguments and may change EAX, ECX and EDX.
    fn after_call(&mut self) {
        self.pushes.clear();
        self.stores.clear();
        self.regs[..3].fill(None);
    }
}

/// The 32-bit register an instruction writes when it names `register`.
fn written(register: Register) -> Option<usize> {
    use Register as R;
    Some(match register {
        R::AL | R::AH | R::AX | R::EAX => 0,
        R::CL | R::CH | R::CX | R::ECX => 1,
        R::DL | R::DH | R::DX | R::EDX => 2,
        R::BL | R::BH | R::BX | R::EBX => 3,
        R::SP | R::ESP => 4,
        R::BP | R::EBP => 5,
        R::SI | R::ESI => 6,
        R::DI | R::EDI => 7,
        _ => return None,
    })
}

fn full(register: Register) -> Option<usize> {
    use Register as R;
    matches!(
        register,
        R::EAX | R::ECX | R::EDX | R::EBX | R::ESP | R::EBP | R::ESI | R::EDI
    )
    .then(|| written(register))
    .flatten()
}

fn absolute(ins: &Instruction) -> Option<u32> {
    (ins.memory_base() == Register::None
        && ins.memory_index() == Register::None
        && !matches!(ins.segment_prefix(), Register::FS | Register::GS))
    .then(|| ins.memory_displacement32())
}

fn value(
    ins: &Instruction,
    operand: u32,
    state: &State,
    slots: &HashMap<u32, Lookup>,
) -> Option<Value> {
    match ins.op_kind(operand) {
        OpKind::Immediate32 => Some(Value::Const(ins.immediate32())),
        OpKind::Immediate8to32 => Some(Value::Const(ins.immediate8to32() as u32)),
        OpKind::Register => full(ins.op_register(operand)).and_then(|r| state.regs[r]),
        OpKind::Memory => absolute(ins)
            .filter(|slot| slots.contains_key(slot))
            .map(Value::Slot),
        _ => None,
    }
}

/// Instructions that change registers they do not name as their first operand.
const IMPLICIT: &[Mnemonic] = &[
    Mnemonic::Cdq,
    Mnemonic::Cwde,
    Mnemonic::Mul,
    Mnemonic::Imul,
    Mnemonic::Div,
    Mnemonic::Idiv,
    Mnemonic::Rdtsc,
    Mnemonic::Cpuid,
    Mnemonic::Lahf,
    Mnemonic::Popad,
    Mnemonic::Popa,
    Mnemonic::Movsb,
    Mnemonic::Movsw,
    Mnemonic::Movsd,
    Mnemonic::Stosb,
    Mnemonic::Stosw,
    Mnemonic::Stosd,
    Mnemonic::Lodsb,
    Mnemonic::Lodsw,
    Mnemonic::Lodsd,
    Mnemonic::Scasb,
    Mnemonic::Scasd,
    Mnemonic::Cmpsb,
    Mnemonic::Cmpsd,
    Mnemonic::Cmpxchg,
];

/// Records the constant names reaching each lookup call in `code`. A linear
/// sweep keeps register and argument constants only within straight-line
/// code; jumps and returns forget them, conditional branches do not.
fn scan(
    code: &[u8],
    ip: u32,
    slots: &HashMap<u32, Lookup>,
    image: &Image<'_>,
    manifest: &mut Manifest,
) {
    // Import thunks: `jmp [slot]`, called directly by incremental links.
    let mut thunks = HashMap::new();
    for (offset, window) in code.windows(6).enumerate() {
        if window[..2] == [0xff, 0x25] {
            let slot = u32::from_le_bytes([window[2], window[3], window[4], window[5]]);
            if let Some(&kind) = slots.get(&slot) {
                thunks.insert(ip.wrapping_add(offset as u32), kind);
            }
        }
    }
    let mut decoder = Decoder::with_ip(32, code, u64::from(ip), DecoderOptions::NONE);
    let mut ins = Instruction::default();
    let mut state = State::default();
    while decoder.can_decode() {
        decoder.decode_out(&mut ins);
        if ins.is_invalid() {
            state = State::default();
            continue;
        }
        match ins.mnemonic() {
            Mnemonic::Push => {
                let pushed = value(&ins, 0, &state, slots);
                state.pushes.push(pushed);
            }
            mnemonic @ (Mnemonic::Mov | Mnemonic::Lea | Mnemonic::Xor)
                if ins.op0_kind() == OpKind::Register =>
            {
                let register = ins.op0_register();
                let known = full(register).and_then(|_| match mnemonic {
                    Mnemonic::Mov => value(&ins, 1, &state, slots),
                    Mnemonic::Lea => absolute(&ins).map(Value::Const),
                    _ => (ins.op1_kind() == OpKind::Register && ins.op1_register() == register)
                        .then_some(Value::Const(0)),
                });
                if let Some(r) = written(register) {
                    state.regs[r] = known;
                }
            }
            Mnemonic::Mov
                if ins.op0_kind() == OpKind::Memory
                    && ins.memory_base() == Register::ESP
                    && ins.memory_index() == Register::None =>
            {
                let stored = value(&ins, 1, &state, slots);
                state.stores.insert(ins.memory_displacement32(), stored);
            }
            Mnemonic::Call => {
                let target = match ins.op0_kind() {
                    OpKind::Memory => absolute(&ins).and_then(|slot| slots.get(&slot)).copied(),
                    OpKind::Register => {
                        match full(ins.op0_register()).and_then(|r| state.regs[r]) {
                            Some(Value::Slot(slot)) => slots.get(&slot).copied(),
                            _ => None,
                        }
                    }
                    OpKind::NearBranch32 => thunks.get(&ins.near_branch32()).copied(),
                    _ => None,
                };
                if let Some(kind) = target {
                    record(kind, &state, image, manifest);
                }
                state.after_call();
            }
            Mnemonic::Ret
            | Mnemonic::Retf
            | Mnemonic::Jmp
            | Mnemonic::Int3
            | Mnemonic::Iret
            | Mnemonic::Iretd
            | Mnemonic::Hlt => state = State::default(),
            Mnemonic::Cmp | Mnemonic::Test | Mnemonic::Nop => {}
            mnemonic => {
                if IMPLICIT.contains(&mnemonic) {
                    for r in [0, 1, 2, 6, 7] {
                        state.regs[r] = None;
                    }
                }
                for operand in 0..ins.op_count().min(2) {
                    let swaps = matches!(mnemonic, Mnemonic::Xchg | Mnemonic::Xadd);
                    if ins.op_kind(operand) == OpKind::Register
                        && (operand == 0 || swaps)
                        && let Some(r) = written(ins.op_register(operand))
                    {
                        state.regs[r] = None;
                    }
                }
            }
        }
    }
}

fn record(kind: Lookup, state: &State, image: &Image<'_>, manifest: &mut Manifest) {
    manifest.sites += 1;
    let constant = |index| match state.arg(index) {
        Some(Value::Const(value)) => Some(value),
        _ => None,
    };
    let named = match kind {
        Lookup::Load { wide } => constant(0)
            .and_then(|va| image.name(va, wide))
            .map(|name| manifest.loads.insert(module_key(&name)))
            .is_some(),
        Lookup::Query { arg } => constant(arg).is_some(),
        Lookup::Proc => match constant(1) {
            Some(ordinal @ 1..=0xffff) => {
                manifest.lookups.insert(format!("#{ordinal}"));
                true
            }
            Some(va) => image
                .name(va, false)
                .map(|name| manifest.lookups.insert(name))
                .is_some(),
            None => false,
        },
        Lookup::Class => match constant(0).and_then(|va| image.at(va).get(..16)) {
            Some(bytes) if bytes.iter().any(|&b| b != 0) => {
                manifest.classes.insert(bytes.try_into().unwrap());
                true
            }
            _ => false,
        },
    };
    if !named {
        manifest.open_sites += 1;
    }
}

/// The loader's key for a module name: its lowercase file name, with `.dll`
/// added when it has no extension.
fn module_key(name: &str) -> String {
    let file = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let mut key = file.to_ascii_lowercase();
    if !key.contains('.') {
        key.push_str(".dll");
    }
    key
}

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` for GUID bytes in memory order.
pub fn guid(bytes: &[u8; 16]) -> String {
    let tail: String = bytes[10..].iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{tail}}}",
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_le_bytes([bytes[4], bytes[5]]),
        u16::from_le_bytes([bytes[6], bytes[7]]),
        bytes[8],
        bytes[9],
    )
}

/// Where a runtime finds what modules require.
pub trait Provider {
    /// The loader maps this module from the host even when a game file has it.
    fn replaces(&self, module: &str) -> bool;
    /// The loader provides this module when no game file has it.
    fn provides(&self, module: &str) -> bool;
    /// A static import binds to an implementation or a reviewed failure.
    fn binds(&self, module: &str, name: &str) -> bool;
    /// Host modules declaring `name`, each with whether a lookup there is
    /// answered as on Windows 7.
    fn lookup(&self, name: &str) -> Vec<(String, bool)>;
    fn class(&self, clsid: &[u8; 16]) -> bool;
}

/// Every requirement the runtime cannot satisfy, each with the files needing it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Gaps {
    /// Statically imported modules that neither the game nor the host has.
    pub missing_modules: BTreeMap<String, BTreeSet<String>>,
    /// `module!name` imports that bind to no implementation or export.
    pub missing_imports: BTreeMap<String, BTreeSet<String>>,
    /// Modules loaded by constant name that neither the game nor the host has.
    pub absent_loads: BTreeMap<String, BTreeSet<String>>,
    /// `module!name` lookups answered differently than on Windows 7.
    pub unavailable_lookups: BTreeMap<String, BTreeSet<String>>,
    /// Lookups of names no game or known host module declares: interfaces
    /// of plugins outside the survey, or names Windows never exported.
    pub undeclared_lookups: BTreeMap<String, BTreeSet<String>>,
    /// CLSIDs with no installed class factory.
    pub missing_classes: BTreeMap<String, BTreeSet<String>>,
    pub sites: u32,
    pub open_sites: u32,
}

fn need(gaps: &mut BTreeMap<String, BTreeSet<String>>, item: String, file: &str) {
    gaps.entry(item).or_default().insert(file.to_owned());
}

pub fn gaps(modules: &[Module], host: &dyn Provider) -> Gaps {
    let local: HashMap<String, &Manifest> = modules
        .iter()
        .map(|module| (module_key(&module.file), &module.manifest))
        .collect();
    let exported = |name: &str| modules.iter().any(|m| m.manifest.exports.contains(name));
    let mut gaps = Gaps::default();
    for module in modules {
        let (manifest, file) = (&module.manifest, module.file.as_str());
        gaps.sites += manifest.sites;
        gaps.open_sites += manifest.open_sites;
        for (dll, name) in &manifest.imports {
            let game = local.get(dll).filter(|_| !host.replaces(dll));
            let bound = match game {
                Some(target) => target.exports.contains(name),
                None if host.replaces(dll) || host.provides(dll) => host.binds(dll, name),
                None => {
                    need(&mut gaps.missing_modules, dll.clone(), file);
                    continue;
                }
            };
            if !bound {
                need(&mut gaps.missing_imports, format!("{dll}!{name}"), file);
            }
        }
        for dll in &manifest.loads {
            if !local.contains_key(dll) && !host.replaces(dll) && !host.provides(dll) {
                need(&mut gaps.absent_loads, dll.clone(), file);
            }
        }
        for name in manifest.lookups.iter().filter(|name| !exported(name)) {
            let candidates = host.lookup(name);
            if candidates.is_empty() {
                need(&mut gaps.undeclared_lookups, name.clone(), file);
            } else if candidates.iter().all(|(_, answered)| !answered) {
                for (dll, _) in candidates {
                    need(&mut gaps.unavailable_lookups, format!("{dll}!{name}"), file);
                }
            }
        }
        for class in &manifest.classes {
            if !host.class(class) {
                need(&mut gaps.missing_classes, guid(class), file);
            }
        }
    }
    gaps
}

/// The runtime as a [`Provider`]: its registry, loader and COM classes.
struct Host<'a> {
    registry: &'a Registry,
    sys: &'a adv_machine::Sys,
    classes: Option<&'a adv_os::com::ClassFactories>,
    /// Name to the modules whose signatures or registrations declare it.
    declared: HashMap<&'a str, BTreeSet<String>>,
    /// Functions Windows 7 lacks, by module; a failed lookup is their native answer.
    later: HashMap<&'static str, BTreeSet<&'static str>>,
}

impl Provider for Host<'_> {
    fn replaces(&self, module: &str) -> bool {
        self.registry.replaces_module(module)
    }
    fn provides(&self, module: &str) -> bool {
        adv_os::loader::is_host_module(self.sys, module)
    }
    fn binds(&self, module: &str, name: &str) -> bool {
        self.registry.is_implemented(module, name)
            || matches!(
                self.registry.classify(module, name),
                Some(ExportClass::Implemented | ExportClass::Reviewed)
            )
    }
    fn lookup(&self, name: &str) -> Vec<(String, bool)> {
        self.declared.get(name).map_or_else(Vec::new, |modules| {
            modules
                .iter()
                .map(|module| {
                    let answered = self.registry.dynamic_available(module, name)
                        || self
                            .later
                            .get(module.as_str())
                            .is_some_and(|names| names.contains(name));
                    (module.clone(), answered)
                })
                .collect()
        })
    }
    fn class(&self, clsid: &[u8; 16]) -> bool {
        self.classes
            .is_some_and(|classes| classes.get(clsid).is_some())
    }
}

impl<B: Backend> Runtime<B> {
    /// The gaps of `modules` against this runtime, before any guest code runs.
    pub fn survey(&self, modules: &[Module]) -> Gaps {
        let registry = self.api_registry();
        let mut declared: HashMap<&str, BTreeSet<String>> = HashMap::new();
        for signature in adv_abi::signature::signatures() {
            declared
                .entry(signature.name)
                .or_default()
                .insert(signature.module.to_ascii_lowercase());
        }
        for (module, name, _) in registry.registered() {
            declared
                .entry(name)
                .or_default()
                .insert(module.to_ascii_lowercase());
        }
        let mut later: HashMap<&str, BTreeSet<&str>> = HashMap::new();
        for line in include_str!("../../../data/abi/post-win7.tsv").lines() {
            let mut fields = line.split('\t');
            if let (Some(module), Some(name)) = (fields.next(), fields.next())
                && !module.starts_with('#')
            {
                later.entry(module).or_default().insert(name);
            }
        }
        let host = Host {
            registry,
            sys: &self.machine.sys,
            classes: self.machine.ext.try_get(),
            declared,
            later,
        };
        gaps(modules, &host)
    }
}

#[cfg(test)]
mod tests;
