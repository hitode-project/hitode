//! Relocatable Tier 2 images. No instruction lifting or backend compilation runs
//! on restoration; current-process symbols and recovery owners are rebuilt.
use super::*;
use cranelift_codegen::binemit::Reloc;
use cranelift_module::{FuncId, ModuleReloc, ModuleRelocTarget};
use sha2::{Digest, Sha256};
use std::sync::{OnceLock, atomic::AtomicUsize};

#[derive(Default)]
pub(super) struct Bindings {
    pub tail: AtomicUsize,
    pub kernels: std::sync::Mutex<Vec<usize>>,
}

const MAGIC: &[u8; 8] = b"ADVNAT04";
const MAX_IMAGE: usize = 16 * 1024 * 1024;
const MAX_ITEMS: usize = 65_536;
const MAX_KEY: usize = 256 * 1024;
const HELPER_NAMES: &[&str] = &[
    "adv_trap_call",
    "adv_repeat",
    "adv_x87",
    "adv_atomic_store",
    "adv_file_read",
    "adv_file_write",
    "adv_file_prepare",
    "adv_code_store",
    "adv_file_write_prepared",
    "adv_range_ok",
    "adv_nest_run",
    "adv_read_timestamp",
    "adv_cache_tail",
    "adv_cache_heat",
];

pub(super) fn capture_enabled(snapshot: &Snapshot) -> bool {
    snapshot.cache_capture || cfg!(test)
}

pub(super) fn x87_operations(
    snapshot: &Snapshot,
    instructions: &BTreeMap<u32, Instruction>,
) -> Box<[crate::floating::NativeX87]> {
    instructions
        .values()
        .filter(|i| !supported(i) && crate::floating::native_x87(i))
        .map(|instruction| {
            let start = (instruction.ip32() - snapshot.origin) as usize;
            let opcode = snapshot.bytes[start..start + instruction.len()]
                .windows(2)
                .find(|bytes| (0xd8..=0xdf).contains(&bytes[0]))
                .map(|bytes| ((bytes[0] as u16 & 7) << 8) | bytes[1] as u16);
            crate::floating::NativeX87 {
                instruction: *instruction,
                opcode,
                fast: crate::floating::fast::classify(instruction),
            }
        })
        .collect()
}

pub(super) fn builder(
    flags: &[(&str, &str)],
    profile: Option<&Arc<Profile>>,
    bindings: Arc<Bindings>,
    x87: &[crate::floating::NativeX87],
) -> Result<JITBuilder, String> {
    let mut builder = JITBuilder::with_flags(flags, default_libcall_names())
        .map_err(|error| error.to_string())?;
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    builder.memory_provider(Box::new(crate::jit_memory::WindowsMemory::default()));
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    builder.memory_provider(Box::new(crate::jit_memory_android::AndroidMemory::default()));
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    builder.memory_provider(Box::new(crate::jit_memory_macos::MacMemory::default()));
    builder.symbol("adv_trap_call", crate::trap::trap_call as *const u8);
    builder.symbol("adv_repeat", crate::strings::repeat as *const u8);
    builder.symbol("adv_x87", x87::helper_address());
    builder.symbol("adv_atomic_store", crate::atomic_memory::store as *const u8);
    builder.symbol("adv_file_read", crate::file_memory::read as *const u8);
    builder.symbol("adv_file_write", crate::file_memory::write as *const u8);
    builder.symbol("adv_file_prepare", crate::file_memory::prepare as *const u8);
    builder.symbol(
        "adv_code_store",
        crate::file_memory::code_store as *const u8,
    );
    builder.symbol(
        "adv_file_write_prepared",
        crate::file_memory::write_prepared as *const u8,
    );
    builder.symbol("adv_range_ok", loops::range_ok as *const u8);
    builder.symbol("adv_nest_run", crate::rows::run as *const u8);
    builder.symbol(
        "adv_read_timestamp",
        crate::trap::read_timestamp as *const u8,
    );
    if let Some(profile) = profile {
        builder.symbol("adv_cache_heat", std::ptr::from_ref(&profile.heat).cast());
        for (index, cell) in profile.calls.iter().enumerate() {
            builder.symbol(
                format!("adv_cache_call_{index}"),
                std::ptr::from_ref(cell).cast(),
            );
        }
    }
    for (index, operation) in x87.iter().enumerate() {
        builder.symbol(
            format!("adv_cache_x87_{index}"),
            std::ptr::from_ref(operation).cast(),
        );
    }
    // The wrapper's tail address is unavailable until body finalization. An
    // import keeps it an absolute relocation on both host architectures.
    builder.symbol_lookup_fn(Box::new(move |name| {
        let address = if name == "adv_cache_tail" {
            Some(bindings.tail.load(Ordering::Relaxed))
        } else {
            name.strip_prefix("adv_cache_kernel_")
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| bindings.kernels.lock().unwrap().get(index).copied())
        };
        address
            .filter(|&address| address != 0)
            .map(|address| address as *const u8)
    }));
    Ok(builder)
}

pub(super) fn declare_profile(
    module: &mut JITModule,
    enabled: bool,
) -> Result<Option<(FuncId, Vec<FuncId>)>, String> {
    if !enabled {
        return Ok(None);
    }
    let signature = module.make_signature();
    let heat = module
        .declare_function("adv_cache_heat", Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    let calls = (0..CALL_SITES)
        .map(|index| {
            module
                .declare_function(
                    &format!("adv_cache_call_{index}"),
                    Linkage::Import,
                    &signature,
                )
                .map_err(|error| error.to_string())
        })
        .collect::<Result<_, _>>()?;
    Ok(Some((heat, calls)))
}

fn isa_key() -> Result<&'static [u8], String> {
    static KEY: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    KEY.get_or_init(|| {
        let module = JITModule::new(
            JITBuilder::with_flags(&[], default_libcall_names())
                .map_err(|error| error.to_string())?,
        );
        Ok(format!(
            "{}\n{}\n{}",
            module.isa().triple(),
            module.isa().flags(),
            module
                .isa()
                .isa_flags()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
        .into_bytes())
    })
    .as_ref()
    .map(Vec::as_slice)
    .map_err(Clone::clone)
}

pub(super) fn key(snapshot: &Snapshot) -> Result<Vec<u8>, String> {
    let mut w = Writer::default();
    w.bytes(MAGIC);
    w.bytes(b"gir-canonical-contract-v3");
    w.bytes(&crate::code_cache::host_build_identity()?);
    w.bytes(env!("ADV_CPU_CODE_CACHE_FINGERPRINT").as_bytes());
    w.bytes(isa_key()?);
    w.bytes(std::env::consts::OS.as_bytes());
    w.bytes(std::env::consts::ARCH.as_bytes());
    w.u32(std::mem::size_of::<usize>() as u32);
    w.u32(std::mem::size_of::<NativeContext>() as u32);
    w.u32(std::mem::size_of::<CpuState>() as u32);
    w.u32(snapshot.base);
    w.u64(snapshot.len as u64);
    w.u32(snapshot.origin);
    w.bool(snapshot.profile.is_some());
    w.bool(cfg!(test));
    w.bool(cfg!(debug_assertions));
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    w.bool(snapshot.native_fault_reads);
    w.bool(verifier_enabled());
    w.bool(x87::enabled());
    w.bool(std::env::var_os("ADV_JIT_X87_SINGLE_ONLY").is_some_and(|value| value == "1"));
    w.bool(cfg!(feature = "gir-capture"));
    w.bool(coverage::enabled());
    w.bool(snapshot.cache_location.is_some());
    if let Some((base, relative, identity)) = snapshot.cache_location {
        w.u32(base);
        w.u32(relative);
        w.bytes(&identity);
    }
    w.bool(snapshot.cache_guest.is_some());
    if let Some(identity) = snapshot.cache_guest {
        w.bytes(&identity);
    }
    let region = &snapshot.region;
    w.u32(region.start);
    w.u64(region.len as u64);
    w.u32(match region.compile_strategy {
        CompileStrategy::Fast => 0,
        CompileStrategy::Speed => 1,
    });
    w.bool(region.reachable);
    w.bool(region.complete_tail);
    w.list(&region.entries);
    w.list(&region.frontier);
    w.list(&region.callees);
    // Generations belong to one Memory owner and are replaced on restoration.
    // Permission and negative mapping dependencies remain semantic inputs.
    w.u32(snapshot.pages.len() as u32);
    for page in &snapshot.pages {
        w.u64(page.page as u64);
        w.u8(page.permissions);
        w.bool(page.mapped);
    }
    w.bytes(&snapshot.bytes);
    if w.0.len() > MAX_KEY {
        return Err("native cache key exceeds limit".into());
    }
    Ok(w.0)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn capture(
    snapshot: &Snapshot,
    module: &JITModule,
    body: FuncId,
    wrapper: FuncId,
    body_context: &cranelift_codegen::Context,
    wrapper_context: &cranelift_codegen::Context,
    kernels: &[(FuncId, cranelift_codegen::Context)],
    instructions: &BTreeMap<u32, Instruction>,
    pages: &BTreeSet<usize>,
    entries: &BTreeSet<u32>,
    excluded: &BTreeMap<u32, iced_x86::Code>,
    code_size: usize,
) -> Result<Vec<u8>, String> {
    let mut w = Writer::default();
    w.bytes(MAGIC);
    w.bytes(&key(snapshot)?);
    w.u32(body.as_u32());
    w.u32(wrapper.as_u32());
    w.u64(code_size as u64);
    let declarations: Vec<_> = module.declarations().get_functions().collect();
    w.u32(declarations.len() as u32);
    for (id, declaration) in declarations {
        w.u32(id.as_u32());
        let name = declaration
            .name
            .as_deref()
            .ok_or("anonymous native cache symbol")?;
        w.bytes(name.as_bytes());
        let import = declaration.linkage == Linkage::Import;
        if !import && declaration.linkage != Linkage::Local {
            return Err("unsupported native symbol linkage".into());
        }
        if import && !known_import(name, snapshot.profile.is_some()) {
            return Err("unsupported native import symbol".into());
        }
        w.bool(import);
    }
    w.list(&instructions.keys().copied().collect::<Vec<_>>());
    w.list(&entries.iter().copied().collect::<Vec<_>>());
    w.list(&excluded.keys().copied().collect::<Vec<_>>());
    w.u32(pages.len() as u32);
    for page in pages {
        w.u64(*page as u64);
    }
    // All body/local functions form one batch; the wrapper is finalized after
    // its absolute tail symbol has been bound to that batch's restored entry.
    w.u32(kernels.len() as u32 + 2);
    for (id, context) in std::iter::once((body, body_context))
        .chain(kernels.iter().map(|(id, context)| (*id, context)))
        .chain(std::iter::once((wrapper, wrapper_context)))
    {
        let code = context
            .compiled_code()
            .ok_or("native cache function is missing")?;
        w.u32(id.as_u32());
        w.bytes(code.code_buffer());
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        {
            let Some(cranelift_codegen::isa::unwind::UnwindInfo::WindowsX64(info)) = code
                .create_unwind_info(module.isa())
                .map_err(|error| error.to_string())?
            else {
                return Err("native cache has no Windows unwind info".into());
            };
            let mut bytes = vec![0; info.emit_size()];
            info.emit(&mut bytes);
            w.u8(bytes[1]);
        }
        #[cfg(not(all(windows, target_arch = "x86_64", target_env = "msvc")))]
        w.u8(0);
        let relocations = code.buffer.relocs();
        w.u32(relocations.len() as u32);
        for relocation in relocations {
            let relocation = ModuleReloc::from_mach_reloc(relocation, &context.func, id);
            w.u32(relocation.offset);
            w.u8(reloc_tag(relocation.kind)?);
            w.u64(relocation.addend as u64);
            match relocation.name {
                ModuleRelocTarget::User {
                    namespace: 0,
                    index,
                } => {
                    w.u8(0);
                    w.u32(index);
                }
                ModuleRelocTarget::FunctionOffset(id, offset) => {
                    w.u8(1);
                    w.u32(id.as_u32());
                    w.u32(offset);
                }
                ModuleRelocTarget::LibCall(call) => {
                    w.u8(2);
                    w.bytes(call.to_string().as_bytes());
                }
                _ => return Err("unsupported native cache relocation target".into()),
            }
        }
    }
    if w.0.len() > MAX_IMAGE {
        return Err("native cache image exceeds limit".into());
    }
    let checksum = checksum(&w.0);
    w.0.extend(checksum);
    Ok(w.0)
}

struct Declaration {
    name: String,
    import: bool,
}
struct Blob {
    id: usize,
    bytes: Vec<u8>,
    unwind_end: u8,
    relocations: Vec<ModuleReloc>,
}
struct Image {
    body: usize,
    wrapper: usize,
    code_size: usize,
    declarations: Vec<Declaration>,
    instructions: BTreeMap<u32, Instruction>,
    pages: BTreeSet<usize>,
    entries: BTreeSet<u32>,
    excluded: BTreeMap<u32, iced_x86::Code>,
    blobs: Vec<Blob>,
}

fn known_import(name: &str, profile: bool) -> bool {
    if name == "adv_cache_heat" {
        return profile;
    }
    if let Some(index) = name
        .strip_prefix("adv_cache_call_")
        .and_then(|index| index.parse::<usize>().ok())
    {
        return profile && index < CALL_SITES;
    }
    for prefix in ["adv_cache_x87_", "adv_cache_kernel_"] {
        if let Some(index) = name
            .strip_prefix(prefix)
            .and_then(|index| index.parse::<usize>().ok())
        {
            return index < MAX_ITEMS;
        }
    }
    HELPER_NAMES.contains(&name)
}

fn parse(snapshot: &Snapshot, bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() < 32 || bytes.len() > MAX_IMAGE + 32 {
        return Err("invalid native cache image size".into());
    }
    let (payload, footer) = bytes.split_at(bytes.len() - 32);
    if checksum(payload).as_slice() != footer {
        return Err("native cache checksum mismatch".into());
    }
    let mut r = Reader(payload);
    if r.bytes(8)? != MAGIC || r.bytes(MAX_KEY)? != key(snapshot)? {
        return Err("native cache input key mismatch".into());
    }
    let body = r.usize32()?;
    let wrapper = r.usize32()?;
    let code_size = r.usize64()?;
    if code_size == 0 || code_size > MAX_IMAGE {
        return Err("invalid native cache code size".into());
    }
    let mut declarations = Vec::new();
    let mut names = BTreeSet::new();
    for index in 0..r.count()? {
        if r.usize32()? != index {
            return Err("noncontiguous native cache symbols".into());
        }
        let name = std::str::from_utf8(r.bytes(128)?)
            .map_err(|_| "invalid native cache symbol")?
            .to_string();
        let import = r.bool()?;
        if !names.insert(name.clone())
            || (import && !known_import(&name, snapshot.profile.is_some()))
            || (!import && !name.starts_with("guest_"))
        {
            return Err("invalid native cache symbol".into());
        }
        declarations.push(Declaration { name, import });
    }
    if body == wrapper
        || declarations.get(body).is_none_or(|decl| decl.import)
        || declarations.get(wrapper).is_none_or(|decl| decl.import)
    {
        return Err("invalid native cache entry symbols".into());
    }
    let mut instructions = BTreeMap::new();
    for pc in r.list()? {
        let offset = pc
            .checked_sub(snapshot.origin)
            .ok_or("invalid native cache instruction")? as usize;
        let bytes = snapshot
            .bytes
            .get(offset..)
            .ok_or("invalid native cache instruction")?;
        let instruction = Decoder::with_ip(32, bytes, u64::from(pc), DecoderOptions::NONE).decode();
        if instructions.insert(pc, instruction).is_some() {
            return Err("duplicate native cache instruction".into());
        }
    }
    if instructions.is_empty() {
        return Err("empty native cache instruction table".into());
    }
    let entries = r.list()?.into_iter().collect::<BTreeSet<_>>();
    if entries.iter().any(|pc| !instructions.contains_key(pc)) {
        return Err("invalid native cache guest entry".into());
    }
    let mut excluded = BTreeMap::new();
    for pc in r.list()? {
        let instruction = instructions
            .get(&pc)
            .ok_or("invalid native cache excluded entry")?;
        excluded.insert(pc, instruction.code());
    }
    let mut pages = BTreeSet::new();
    for _ in 0..r.count()? {
        let page = r.usize64()?;
        if !snapshot.pages.iter().any(|stamp| stamp.page == page) || !pages.insert(page) {
            return Err("invalid native cache page dependency".into());
        }
    }
    let mut blobs = Vec::new();
    let mut defined = BTreeSet::new();
    let mut actual_size = 0usize;
    for _ in 0..r.count()? {
        let id = r.usize32()?;
        if declarations.get(id).is_none_or(|decl| decl.import) || !defined.insert(id) {
            return Err("invalid native cache definition".into());
        }
        let bytes = r.bytes(MAX_IMAGE)?.to_vec();
        if bytes.is_empty() {
            return Err("empty native cache definition".into());
        }
        actual_size = actual_size
            .checked_add(bytes.len())
            .ok_or("native cache size overflow")?;
        let unwind_end = r.u8()?;
        let mut relocations = Vec::new();
        for _ in 0..r.count()? {
            let offset = r.u32()?;
            let kind = reloc_kind(r.u8()?)?;
            let addend = r.u64()? as i64;
            if offset as usize + reloc_width(kind) > bytes.len() {
                return Err("native cache relocation outside code".into());
            }
            let name = match r.u8()? {
                0 => ModuleRelocTarget::User {
                    namespace: 0,
                    index: r.u32()?,
                },
                1 => ModuleRelocTarget::FunctionOffset(FuncId::from_u32(r.u32()?), r.u32()?),
                2 => ModuleRelocTarget::LibCall(
                    std::str::from_utf8(r.bytes(64)?)
                        .map_err(|_| "invalid native cache libcall")?
                        .parse()
                        .map_err(|_| "invalid native cache libcall")?,
                ),
                _ => return Err("invalid native cache relocation target".into()),
            };
            if addend.unsigned_abs() > MAX_IMAGE as u64 {
                return Err("native cache relocation addend exceeds limit".into());
            }
            relocations.push(ModuleReloc {
                offset,
                kind,
                name,
                addend,
            });
        }
        blobs.push(Blob {
            id,
            bytes,
            unwind_end,
            relocations,
        });
    }
    if !r.0.is_empty()
        || actual_size != code_size
        || !defined.contains(&body)
        || blobs.last().is_none_or(|blob| blob.id != wrapper)
        || declarations
            .iter()
            .enumerate()
            .any(|(id, decl)| !decl.import && !defined.contains(&id))
    {
        return Err("incomplete native cache image".into());
    }
    let x87_count = instructions
        .values()
        .filter(|i| !supported(i) && crate::floating::native_x87(i))
        .count();
    for declaration in &declarations {
        if let Some(index) = declaration.name.strip_prefix("adv_cache_x87_") {
            let index: usize = index
                .parse()
                .map_err(|_| "invalid native cache x87 symbol")?;
            if index >= x87_count {
                return Err("native cache x87 symbol has no operation".into());
            }
        }
        if let Some(index) = declaration.name.strip_prefix("adv_cache_kernel_") {
            let index: usize = index
                .parse()
                .map_err(|_| "invalid native cache kernel symbol")?;
            if index >= MAX_ITEMS
                || !blobs
                    .iter()
                    .any(|blob| kernel_index(&declarations[blob.id].name) == Some(index))
            {
                return Err("native cache kernel symbol has no function".into());
            }
        }
        if kernel_index(&declaration.name).is_some_and(|index| index >= MAX_ITEMS) {
            return Err("native cache kernel index exceeds limit".into());
        }
    }
    for blob in &blobs {
        for relocation in &blob.relocations {
            match relocation.name {
                ModuleRelocTarget::User { index, .. } => {
                    let target = declarations
                        .get(index as usize)
                        .ok_or("invalid native cache target symbol")?;
                    if target.import && relocation.kind != Reloc::Abs8 {
                        return Err("native cache import requires an absolute relocation".into());
                    }
                }
                ModuleRelocTarget::FunctionOffset(id, offset) => {
                    if blobs
                        .iter()
                        .find(|blob| blob.id == id.as_u32() as usize)
                        .is_none_or(|blob| offset as usize >= blob.bytes.len())
                    {
                        return Err("native cache function offset is outside code".into());
                    }
                }
                ModuleRelocTarget::LibCall(_) if relocation.kind == Reloc::Abs8 => {}
                _ => return Err("unsupported native cache target relocation".into()),
            }
        }
    }
    Ok(Image {
        body,
        wrapper,
        code_size,
        declarations,
        instructions,
        pages,
        entries,
        excluded,
        blobs,
    })
}

/// Restore only after a complete key, integrity and relocation validation.
/// Current Memory generations and runtime helper addresses are never read from disk.
pub(crate) fn restore_snapshot(snapshot: &Snapshot, bytes: &[u8]) -> Result<Compiled, String> {
    let image = parse(snapshot, bytes)?;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let _write = crate::jit_memory_macos::WriteScope::new();
    let bindings = Arc::new(Bindings::default());
    let x87 = x87_operations(snapshot, &image.instructions);
    let builder = builder(&[], snapshot.profile.as_ref(), bindings.clone(), &x87)?;
    let mut owned = OwnedModule(Some(JITModule::new(builder)));
    let module = owned.0.as_mut().unwrap();
    let signature = module.make_signature();
    for (index, declaration) in image.declarations.iter().enumerate() {
        let id = module
            .declare_function(
                &declaration.name,
                if declaration.import {
                    Linkage::Import
                } else {
                    Linkage::Local
                },
                &signature,
            )
            .map_err(|error| error.to_string())?;
        if id.as_u32() as usize != index {
            return Err("native cache declaration mismatch".into());
        }
    }
    let wrapper = image.blobs.last().unwrap();
    let is_kernel = |blob: &Blob| kernel_index(&image.declarations[blob.id].name).is_some();
    for blob in image.blobs.iter().filter(|blob| is_kernel(blob)) {
        module
            .define_function_bytes(
                FuncId::from_u32(blob.id as u32),
                16,
                &blob.bytes,
                &blob.relocations,
            )
            .map_err(|error| error.to_string())?;
    }
    finalize(module)?;
    {
        let mut kernels = bindings.kernels.lock().unwrap();
        for blob in image.blobs.iter().filter(|blob| is_kernel(blob)) {
            let index = kernel_index(&image.declarations[blob.id].name).unwrap();
            if kernels.len() <= index {
                kernels.resize(index + 1, 0);
            }
            kernels[index] =
                module.get_finalized_function(FuncId::from_u32(blob.id as u32)) as usize;
        }
    }
    for blob in image.blobs[..image.blobs.len() - 1]
        .iter()
        .filter(|blob| !is_kernel(blob))
    {
        module
            .define_function_bytes(
                FuncId::from_u32(blob.id as u32),
                16,
                &blob.bytes,
                &blob.relocations,
            )
            .map_err(|error| error.to_string())?;
    }
    // JIT relocation may reject an unforeseen host allocation distance. This
    // remains a cache miss; no entry is published while either batch is pending.
    finalize(module)?;
    let body_entry = module.get_finalized_function(FuncId::from_u32(image.body as u32)) as usize;
    bindings.tail.store(body_entry, Ordering::Relaxed);
    module
        .define_function_bytes(
            FuncId::from_u32(wrapper.id as u32),
            16,
            &wrapper.bytes,
            &wrapper.relocations,
        )
        .map_err(|error| error.to_string())?;
    finalize(module)?;
    let entry = module.get_finalized_function(FuncId::from_u32(image.wrapper as u32));
    let mut perf_ranges = Vec::new();
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let mut unwind = Vec::new();
    for blob in &image.blobs {
        let address = module.get_finalized_function(FuncId::from_u32(blob.id as u32)) as usize;
        if let Some(registration) =
            crate::perf_map::register(address, blob.bytes.len(), crate::perf_map::Category::Tier2)
        {
            perf_ranges.push(registration);
        }
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        unwind.push(unsafe {
            crate::host_fault::UnwindRegistration::cached(
                address,
                blob.bytes.len(),
                blob.unwind_end,
            )
        }?);
    }
    let input = snapshot.with_coverage(&image.instructions, &image.pages);
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    let body_len = image
        .blobs
        .iter()
        .find(|blob| blob.id == image.body)
        .unwrap()
        .bytes
        .len();
    Ok(Compiled {
        perf_ranges,
        module: owned.0.take(),
        cache_image: Some(Ok(bytes.to_vec())),
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        unwind,
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        fault_sites: Vec::new(),
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        fault_recipes: Vec::new(),
        #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
        body_len,
        entry: unsafe { std::mem::transmute::<*const u8, Entry>(entry) },
        tail_entry: body_entry,
        pages: image
            .pages
            .into_iter()
            .map(|page| (page, snapshot.version(page)))
            .collect(),
        input,
        window_base: snapshot.base,
        window_len: snapshot.len,
        entries: image.entries,
        excluded_entries: image.excluded,
        code_size: image.code_size,
        instruction_count: image.instructions.len(),
        #[cfg(test)]
        gir_ssa_instructions: 0,
        #[cfg(test)]
        gir_opaque_instructions: 0,
        decoded_bytes: image.instructions.values().map(Instruction::len).sum(),
        ir_counts: None,
        _x87: x87,
        _nests: Box::new([]),
        cache_stamp: std::cell::Cell::new(0),
        watched_in: std::cell::Cell::new(0),
        #[cfg(all(test, target_arch = "x86_64"))]
        disasm: String::new(),
        #[cfg(all(test, target_arch = "x86_64"))]
        versioned_loops: image.blobs.iter().filter(|blob| is_kernel(blob)).count(),
        #[cfg(all(test, target_arch = "x86_64"))]
        vectorized_loops: 0,
        #[cfg(all(test, target_arch = "x86_64"))]
        invariant_body: image
            .declarations
            .iter()
            .any(|declaration| declaration.name == "guest_invariant"),
    })
}

fn kernel_index(name: &str) -> Option<usize> {
    name.strip_prefix("guest_kernel_")?
        .strip_suffix("_row_false")?
        .parse()
        .ok()
}

fn finalize(module: &mut JITModule) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        module.finalize_definitions()
    }))
    .map_err(|_| "native cache relocation publication rejected".to_string())?
    .map_err(|error| error.to_string())
}
struct OwnedModule(Option<JITModule>);
impl Drop for OwnedModule {
    fn drop(&mut self) {
        // SAFETY: pending modules have never published an executable entry.
        if let Some(module) = self.0.take() {
            unsafe { module.free_memory() };
        }
    }
}

fn reloc_tag(kind: Reloc) -> Result<u8, String> {
    Ok(match kind {
        Reloc::Abs8 => 0,
        Reloc::X86PCRel4 => 1,
        Reloc::X86CallPCRel4 => 2,
        Reloc::Arm64Call => 3,
        Reloc::Aarch64AdrPrelPgHi21 => 4,
        Reloc::Aarch64AddAbsLo12Nc => 5,
        _ => return Err("unsupported native cache relocation kind".into()),
    })
}
fn reloc_kind(tag: u8) -> Result<Reloc, String> {
    Ok(match tag {
        0 => Reloc::Abs8,
        1 => Reloc::X86PCRel4,
        2 => Reloc::X86CallPCRel4,
        3 => Reloc::Arm64Call,
        4 => Reloc::Aarch64AdrPrelPgHi21,
        5 => Reloc::Aarch64AddAbsLo12Nc,
        _ => return Err("invalid native cache relocation kind".into()),
    })
}
fn reloc_width(kind: Reloc) -> usize {
    if kind == Reloc::Abs8 { 8 } else { 4 }
}
fn checksum(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
#[derive(Default)]
struct Writer(Vec<u8>);
impl Writer {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }
    fn u32(&mut self, value: u32) {
        self.0.extend(value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend(value.to_le_bytes());
    }
    fn bytes(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.0.extend(value);
    }
    fn list(&mut self, values: &[u32]) {
        self.u32(values.len() as u32);
        for value in values {
            self.u32(*value);
        }
    }
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        if len > self.0.len() {
            return Err("truncated native cache image".into());
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("invalid native cache boolean".into()),
        }
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn usize32(&mut self) -> Result<usize, String> {
        Ok(self.u32()? as usize)
    }
    fn usize64(&mut self) -> Result<usize, String> {
        usize::try_from(self.u64()?).map_err(|_| "native cache size overflow".into())
    }
    fn count(&mut self) -> Result<usize, String> {
        let count = self.usize32()?;
        if count > MAX_ITEMS || count > self.0.len() {
            return Err("native cache table exceeds limit".into());
        }
        Ok(count)
    }
    fn bytes(&mut self, limit: usize) -> Result<&'a [u8], String> {
        let len = self.usize32()?;
        if len > limit {
            return Err("native cache field exceeds limit".into());
        }
        self.take(len)
    }
    fn list(&mut self) -> Result<Vec<u32>, String> {
        (0..self.count()?).map(|_| self.u32()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::{Interpreter, Step};

    fn fixture(bytes: &[u8]) -> (Memory, Snapshot) {
        let mut memory = Memory::new(0x10000, 8192).unwrap();
        memory.protect(0x10000, 8192, READ | WRITE).unwrap();
        memory.write(0x10000, bytes).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        let mut region = Region::new(0x10000, bytes.len());
        region.compile_strategy = CompileStrategy::Fast;
        let snapshot = Snapshot::capture(&region, &mut memory).unwrap();
        (memory, snapshot)
    }

    #[test]
    fn native_image_restores_in_another_process() {
        let (mut memory, mut snapshot) = fixture(&[0xb8, 41, 0, 0, 0, 0x83, 0xc0, 1, 0xcd, 0x2e]);
        snapshot.profile = Some(Arc::new(Profile::default()));
        if let Some(path) = std::env::var_os("ADV_NATIVE_CACHE_CHILD") {
            let image = std::fs::read(path).unwrap();
            if std::env::var_os("ADV_NATIVE_CACHE_REJECT_BINARY").is_some() {
                assert!(restore_snapshot(&snapshot, &image).is_err());
                return;
            }
            let compiled = restore_snapshot(&snapshot, &image).unwrap();
            let mut state = CpuState {
                eip: snapshot.region.start,
                fuel: 100,
                ..CpuState::default()
            };
            assert_eq!(
                x87::testing::run(&compiled, &mut state, &mut memory),
                Exit::Trap as u32
            );
            assert_eq!(state.regs[0], 42);
            assert!(snapshot.profile.as_ref().unwrap().heat() > 0);
            return;
        }
        let compiled = compile_snapshot(&snapshot).unwrap();
        let path =
            std::env::temp_dir().join(format!("adv-native-cache-{}.bin", std::process::id()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let cleanup = Cleanup(path.clone());
        std::fs::write(&path, compiled.cache_bytes().unwrap()).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("compile::cache::tests::native_image_restores_in_another_process")
            .arg("--nocapture")
            .env("ADV_NATIVE_CACHE_CHILD", &path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "child native restore failed: {}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        // Appending invalidates a macOS code signature. Exercise the alternate
        // executable on hosts that allow an unsigned test binary instead.
        #[cfg(not(target_os = "macos"))]
        {
            // A valid executable with a different file identity must reject the
            // artifact even when its machine instructions and guest bytes match.
            let other = path.with_extension("exe");
            std::fs::copy(std::env::current_exe().unwrap(), &other).unwrap();
            let other_cleanup = Cleanup(other.clone());
            {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&other)
                    .unwrap()
                    .write_all(b"different native build identity")
                    .unwrap();
            }
            let result = std::process::Command::new(&other)
                .arg("--exact")
                .arg("compile::cache::tests::native_image_restores_in_another_process")
                .env("ADV_NATIVE_CACHE_CHILD", &path)
                .env("ADV_NATIVE_CACHE_REJECT_BINARY", "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "changed host identity was not rejected: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            drop(other_cleanup);
        }
        drop(cleanup);
    }

    #[test]
    fn native_image_moves_and_rebinds_fresh_profile_and_helper_symbols() {
        // mov eax,41; add eax,1; int 0x2e. The trap helper is an import relocation.
        let (mut memory, mut snapshot) = fixture(&[0xb8, 41, 0, 0, 0, 0x83, 0xc0, 1, 0xcd, 0x2e]);
        let old_profile = Arc::new(Profile::default());
        snapshot.profile = Some(old_profile.clone());
        let compiled = compile_snapshot(&snapshot).unwrap();
        let old_entry = compiled.tail_entry;
        let image = compiled.cache_bytes().unwrap();
        let new_profile = Arc::new(Profile::default());
        snapshot.profile = Some(new_profile.clone());
        let restored = restore_snapshot(&snapshot, &image).unwrap();
        assert_ne!(old_entry, restored.tail_entry);
        let mut state = CpuState {
            eip: snapshot.region.start,
            fuel: 100,
            ..CpuState::default()
        };
        let reason = x87::testing::run(&restored, &mut state, &mut memory);
        assert_eq!(reason, Exit::Trap as u32);
        assert_eq!(state.regs[0], 42);
        assert_eq!(state.eip, snapshot.region.start + 10);
        assert_eq!(old_profile.heat(), 0);
        assert!(new_profile.heat() > 0);
    }

    #[test]
    fn restored_loop_kernel_uses_new_module_addresses() {
        use iced_x86::code_asm::*;
        let mut assembler = CodeAssembler::new(32).unwrap();
        let mut top = assembler.create_label();
        assembler.set_label(&mut top).unwrap();
        assembler.mov(eax, dword_ptr(esi)).unwrap();
        assembler.mov(dword_ptr(edi), eax).unwrap();
        assembler.add(esi, 4).unwrap();
        assembler.add(edi, 4).unwrap();
        assembler.dec(ecx).unwrap();
        assembler.jne(top).unwrap();
        assembler.int(0x2e).unwrap();
        let bytes = assembler.assemble(0x10000).unwrap();
        let (mut memory, mut snapshot) = fixture(&bytes);
        snapshot.region.compile_strategy = CompileStrategy::Speed;
        let compiled = compile_snapshot(&snapshot).unwrap();
        let image = compiled.cache_bytes().unwrap();
        let restored = restore_snapshot(&snapshot, &image).unwrap();
        let (mut oracle_memory, _) = fixture(&bytes);
        let values = [1u32, 2, 3, 4].map(u32::to_le_bytes).concat();
        memory.write(0x11020, &values).unwrap();
        oracle_memory.write(0x11020, &values).unwrap();
        let mut actual = CpuState {
            eip: snapshot.region.start,
            fuel: 100,
            ..CpuState::default()
        };
        actual.regs[1] = 4;
        actual.regs[6] = 0x11020;
        actual.regs[7] = 0x11080;
        let mut expected = actual.clone();
        let mut interpreter = Interpreter::new();
        while interpreter.step(&mut expected, &mut oracle_memory) == Step::Executed {}
        assert_eq!(
            x87::testing::run(&restored, &mut actual, &mut memory),
            Exit::Trap as u32
        );
        expected.fuel = actual.fuel;
        assert_eq!(actual, expected);
        assert_eq!(memory.read_data(0x11080, values.len()).unwrap(), values);
        #[cfg(target_arch = "x86_64")]
        assert!(restored.versioned_loops > 0);
    }

    #[test]
    fn restored_x87_operation_descriptors_preserve_oracle_state() {
        // FLD1; FXAM; FSTP qword [absolute data]. FXAM uses the canonical helper.
        let bytes = [0xd9, 0xe8, 0xd9, 0xe5, 0xdd, 0x1d, 0x20, 0x10, 1, 0];
        let (mut memory, snapshot) = fixture(&bytes);
        let compiled = compile_snapshot(&snapshot).unwrap();
        let image = compiled.cache_bytes().unwrap();
        let restored = restore_snapshot(&snapshot, &image).unwrap();
        let (mut oracle_memory, _) = fixture(&bytes);
        let mut actual = CpuState {
            eip: snapshot.region.start,
            fuel: 100,
            ..CpuState::default()
        };
        let mut expected = actual.clone();
        let mut interpreter = Interpreter::new();
        while expected.eip < snapshot.region.start + bytes.len() as u32 {
            assert_eq!(
                interpreter.step(&mut expected, &mut oracle_memory),
                Step::Executed
            );
        }
        assert_eq!(
            x87::testing::run(&restored, &mut actual, &mut memory),
            Exit::Dispatch as u32
        );
        expected.fuel = actual.fuel;
        assert_eq!(actual, expected);
        assert_eq!(
            memory.read_data(0x11020, 8).unwrap().as_ref(),
            &1f64.to_le_bytes()
        );
    }

    #[test]
    fn restored_checked_store_fault_keeps_the_committed_prefix() {
        let bytes = [0x83, 0xc3, 1, 0x89, 0x06];
        let (mut memory, snapshot) = fixture(&bytes);
        let compiled = compile_snapshot(&snapshot).unwrap();
        let image = compiled.cache_bytes().unwrap();
        let restored = restore_snapshot(&snapshot, &image).unwrap();
        memory.protect(0x11000, 4096, READ).unwrap();
        let mut state = CpuState {
            eip: snapshot.region.start,
            fuel: 100,
            ..CpuState::default()
        };
        state.regs[0] = 77;
        state.regs[3] = 12;
        state.regs[6] = 0x11020;
        assert_eq!(
            x87::testing::run(&restored, &mut state, &mut memory),
            Exit::WriteFault as u32
        );
        assert_eq!(state.regs[3], 13);
        assert_eq!(state.eip, snapshot.region.start + 3);
        assert_eq!(state.fault_addr, 0x11020);
        assert_eq!(memory.read_data(0x11020, 4).unwrap().as_ref(), &[0; 4]);
    }

    #[test]
    fn restored_native_fault_preserves_interpreter_prefix_and_smc_invalidates() {
        // add ebx,1; mov eax,[esi]. An unmapped read must keep the add's prefix.
        let bytes = [0x83, 0xc3, 1, 0x8b, 0x06];
        let (mut memory, snapshot) = fixture(&bytes);
        let compiled = compile_snapshot(&snapshot).unwrap();
        let image = compiled.cache_bytes().unwrap();
        let mut restored = restore_snapshot(&snapshot, &image).unwrap();
        let mut actual = CpuState {
            eip: snapshot.region.start,
            fuel: 100,
            ..CpuState::default()
        };
        actual.regs[0] = 77;
        actual.regs[3] = 12;
        let mut expected = actual.clone();
        let (mut oracle_memory, _) = fixture(&bytes);
        let mut interpreter = Interpreter::new();
        assert_eq!(
            interpreter.step(&mut expected, &mut oracle_memory),
            Step::Executed
        );
        assert!(matches!(
            interpreter.step(&mut expected, &mut oracle_memory),
            Step::ReadFault { .. }
        ));
        assert_eq!(
            x87::testing::run(&restored, &mut actual, &mut memory),
            Exit::ReadFault as u32
        );
        expected.fuel = actual.fuel;
        assert_eq!(actual, expected);
        assert!(restored.valid(&memory));
        memory.write(snapshot.region.start + 2, &[2]).unwrap();
        assert!(!restored.valid(&memory));
        assert!(!restored.revalidate(&memory));
        let current = Snapshot::capture(&snapshot.region, &mut memory).unwrap();
        assert!(restore_snapshot(&current, &image).is_err());
    }

    #[test]
    fn native_image_rejects_corruption_options_permissions_and_location_changes() {
        let (mut memory, snapshot) = fixture(&[0x90, 0x90]);
        let image = compile_snapshot(&snapshot).unwrap().cache_bytes().unwrap();
        for index in [0, image.len() / 2, image.len() - 1] {
            let mut damaged = image.clone();
            damaged[index] ^= 1;
            assert!(restore_snapshot(&snapshot, &damaged).is_err());
        }
        assert!(restore_snapshot(&snapshot, &image[..image.len() - 1]).is_err());
        let mut changed = snapshot.clone();
        changed.cache_location = Some((changed.base, 0, [1; 32]));
        assert!(restore_snapshot(&changed, &image).is_err());
        memory
            .protect(snapshot.region.start, 4096, READ | EXEC)
            .unwrap();
        let changed = Snapshot::capture(&snapshot.region, &mut memory).unwrap();
        assert!(restore_snapshot(&changed, &image).is_err());
    }

    #[test]
    fn identical_code_rejects_other_guest_images_and_bases() {
        let (_, mut snapshot) = fixture(&[0x90, 0x90]);
        snapshot.cache_guest = Some([1; 32]);
        snapshot.cache_location = Some((snapshot.region.start, 0, [2; 32]));
        let image = compile_snapshot(&snapshot).unwrap().cache_bytes().unwrap();
        assert!(restore_snapshot(&snapshot, &image).is_ok());
        let mut changed = snapshot.clone();
        changed.cache_guest = Some([3; 32]);
        assert!(restore_snapshot(&changed, &image).is_err());
        changed = snapshot.clone();
        changed.cache_location.as_mut().unwrap().2 = [4; 32];
        assert!(restore_snapshot(&changed, &image).is_err());
        changed = snapshot.clone();
        changed.cache_location.as_mut().unwrap().0 += 4096;
        assert!(restore_snapshot(&changed, &image).is_err());
    }

    #[test]
    fn current_generations_are_rebuilt_and_reachable_coverage_keeps_original_key() {
        let (mut memory, mut snapshot) = fixture(&[0x90, 0xeb, 2, 0x90, 0x90, 0x90]);
        snapshot.region.reachable = true;
        let mut compiled = compile_snapshot(&snapshot).unwrap();
        let image = compiled.cache_bytes().unwrap();
        memory.write(snapshot.region.start + 4, &[0x91]).unwrap();
        assert!(compiled.revalidate(&memory));
        // A fresh capture changes the conservative key even though published
        // reachable coverage can preserve the old native code after this write.
        let changed = Snapshot::capture(&snapshot.region, &mut memory).unwrap();
        assert!(restore_snapshot(&changed, &image).is_err());
        memory.write(snapshot.region.start + 4, &[0x90]).unwrap();
        let current = Snapshot::capture(&snapshot.region, &mut memory).unwrap();
        assert_eq!(snapshot.cache_key().unwrap(), current.cache_key().unwrap());
        let restored = restore_snapshot(&current, &image).unwrap();
        assert!(restored.valid(&memory));
        assert_eq!(
            restored.pages,
            current
                .pages
                .iter()
                .map(|stamp| (stamp.page, stamp.version))
                .collect::<Vec<_>>()
        );
    }
}
