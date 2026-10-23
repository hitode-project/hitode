//! Loading Unix libraries into the guest, as `dlopen` would.
//!
//! A library is mapped high in the address space, relocated, bound and
//! initialized once. Its undefined symbols resolve, in order, to another
//! loaded library's definition, to ntdll.dll's export of the same name for
//! a symbol of ntdll.so (the Unix and PE sides of ntdll share most of
//! their exports) or of the C library functions ntdll.dll also implements,
//! through a trampoline where the export is stdcall, and to a host
//! function for the rest. A symbol nothing provides binds to a thunk that stops the
//! machine when called, or to 0 when it is weak.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};
use adv_nt::memory::{
    self, Memory, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_READONLY, PAGE_READWRITE,
};
use adv_nt::process::Ntdll;
use adv_nt::status::{self, Status};
use adv_nt::unixlib::Loaded;

use crate::elf::{self, R_386_32, R_386_GLOB_DAT, R_386_JMP_SLOT, R_386_PC32, R_386_RELATIVE};
use crate::{libc, stdcall};

const NTDLL: &str = "ntdll.so";
const FUNCS: &str = "__wine_unix_call_funcs";
const DLL_NOT_FOUND: Status = 0xc000_0135;
const ENTRYPOINT_NOT_FOUND: Status = 0xc000_0139;

pub struct Library {
    pub path: PathBuf,
    pub base: u32,
    exports: HashMap<String, u32>,
}

impl Library {
    /// The address of an exported symbol.
    pub fn symbol(&self, name: &str) -> Option<u32> {
        self.exports.get(name).map(|value| self.base + value)
    }
}

/// The loaded libraries, in load order.
#[derive(Default)]
pub struct Libraries(pub Vec<Library>);

impl Libraries {
    fn find(&self, path: &Path) -> Option<&Library> {
        self.0.iter().find(|library| library.path == path)
    }
}

/// Loads the library at `path` and passes on its `__wine_unix_call_funcs`
/// ([`adv_nt::unixlib::Loader`]).
pub fn load_unixlib(env: &mut Env<'_>, path: &Path, then: Loaded) -> Outcome {
    open(
        env,
        path,
        Box::new(|env, library| {
            let funcs = library.and_then(|base| {
                let libraries = env.ext.get_or_default::<Libraries>();
                let library = libraries.0.iter().find(|library| library.base == base);
                library
                    .and_then(|library| library.symbol(FUNCS))
                    .ok_or(ENTRYPOINT_NOT_FOUND)
            });
            then(env, funcs)
        }),
    )
}

/// What to do with a loaded library's base, or the reason it did not load.
pub type Opened = Box<dyn FnOnce(&mut Env<'_>, Result<u32, Status>) -> Outcome>;

/// Loads the library at `path`, once, and passes on its base.
pub fn open(env: &mut Env<'_>, path: &Path, then: Opened) -> Outcome {
    if let Some(library) = env.ext.get_or_default::<Libraries>().find(path) {
        let base = library.base;
        return then(env, Ok(base));
    }
    let object = match std::fs::read(path) {
        Ok(data) => match elf::parse(&data) {
            Ok(object) => object,
            Err(error) => {
                log::warn!("{}: {error}", path.display());
                return then(env, Err(status::INVALID_IMAGE_FORMAT));
            }
        },
        Err(_) => return then(env, Err(DLL_NOT_FOUND)),
    };
    let Some((base, protections)) = map(env, &object) else {
        return then(env, Err(status::NO_MEMORY));
    };
    if let Err(error) = relocate(env, &object, base, &protections) {
        return Outcome::Fail(format!("{}: {error}", path.display()));
    }
    let exports = object
        .exports()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    env.ext.get_or_default::<Libraries>().0.push(Library {
        path: path.to_owned(),
        base,
        exports,
    });

    // DT_INIT, then the initializer array, each called with no arguments.
    let mut initializers: VecDeque<u32> = object.init.map(|init| base + init).into_iter().collect();
    if let Some((array, size)) = object.init_array {
        for index in 0..size / 4 {
            match env.cpu.read_u32(base + array + index * 4) {
                Ok(0 | u32::MAX) => {}
                Ok(function) => initializers.push_back(function),
                Err(_) => return then(env, Err(status::INVALID_IMAGE_FORMAT)),
            }
        }
    }
    initialize(env, initializers, Box::new(move |env| then(env, Ok(base))))
}

fn initialize(
    env: &mut Env<'_>,
    mut initializers: VecDeque<u32>,
    then: Box<dyn FnOnce(&mut Env<'_>) -> Outcome>,
) -> Outcome {
    match initializers.pop_front() {
        None => then(env),
        Some(function) => Outcome::call(function, [], move |env, _| {
            initialize(env, initializers, then)
        }),
    }
}

/// Maps the image writable for relocation, and returns its base and the
/// protections its pages get afterwards.
fn map(env: &mut Env<'_>, object: &elf::Object) -> Option<(u32, Vec<u32>)> {
    let protections: Vec<u32> = object
        .pages
        .iter()
        .map(|&(write, execute)| match (write, execute) {
            (false, false) => PAGE_READONLY,
            (true, false) => PAGE_READWRITE,
            (false, true) => PAGE_EXECUTE_READ,
            (true, true) => PAGE_EXECUTE_READWRITE,
        })
        .collect();
    let writable = vec![PAGE_READWRITE; protections.len()];
    let memory = env.ext.get_or_default::<Memory>();
    let base = memory::map_library(env.cpu, &mut env.sys.vm, memory, &object.image, &writable)?;
    Some((base, protections))
}

fn relocate(
    env: &mut Env<'_>,
    object: &elf::Object,
    base: u32,
    protections: &[u32],
) -> Result<(), String> {
    let mut cache: HashMap<u32, u32> = HashMap::new();
    for relocation in &object.relocations {
        let at = base + relocation.offset;
        let word = env.cpu.read_u32(at).map_err(|e| e.to_string())?;
        let value = match relocation.kind {
            R_386_RELATIVE => word.wrapping_add(base),
            R_386_GLOB_DAT | R_386_JMP_SLOT => {
                bind(env, object, base, relocation.symbol, &mut cache)?
            }
            R_386_32 => word.wrapping_add(bind(env, object, base, relocation.symbol, &mut cache)?),
            R_386_PC32 => word
                .wrapping_add(bind(env, object, base, relocation.symbol, &mut cache)?)
                .wrapping_sub(at),
            _ => continue,
        };
        env.cpu.write_u32(at, value).map_err(|e| e.to_string())?;
    }
    let memory = env.ext.get_or_default::<Memory>();
    for (page, &protection) in protections.iter().enumerate() {
        let address = base + page as u32 * elf::PAGE as u32;
        let prot = memory::guest_protection(memory, protection);
        env.cpu
            .protect(address, elf::PAGE as u32, prot)
            .map_err(|e| e.to_string())?;
    }
    memory::set_protections(memory, base, protections);
    Ok(())
}

/// The address symbol `index` of `object`, loaded at `base`, binds to.
fn bind(
    env: &mut Env<'_>,
    object: &elf::Object,
    base: u32,
    index: u32,
    cache: &mut HashMap<u32, u32>,
) -> Result<u32, String> {
    if let Some(&value) = cache.get(&index) {
        return Ok(value);
    }
    let symbol = object
        .symbols
        .get(index as usize)
        .ok_or("relocation symbol out of range")?;
    // A library's own definitions come first, as DF_SYMBOLIC asks.
    let value = if symbol.defined {
        base + symbol.value
    } else {
        resolve(env, symbol)?
    };
    cache.insert(index, value);
    Ok(value)
}

/// The address an undefined symbol binds to.
fn resolve(env: &mut Env<'_>, symbol: &elf::Symbol) -> Result<u32, String> {
    let name = symbol.name.as_str();
    if let Some(address) = env
        .ext
        .get_or_default::<Libraries>()
        .0
        .iter()
        .find_map(|library| library.symbol(name))
    {
        return Ok(address);
    }
    let library = symbol.library.as_deref().unwrap_or(NTDLL);
    let ntdll = |env: &Env<'_>, name| env.ext.try_get::<Ntdll>().and_then(|ntdll| ntdll.get(name));
    match library {
        NTDLL => {
            if let Some(address) = ntdll(env, name) {
                return stdcall::adapt(env, name, address);
            }
        }
        // ntdll.dll's C library functions are cdecl.
        _ => {
            if let Some(address) = libc::shared_with_ntdll(name).and_then(|name| ntdll(env, name)) {
                return Ok(address);
            }
        }
    }
    if symbol.weak && env.sys.thunks.resolver().host_fn(library, name).is_none() {
        return Ok(0);
    }
    env.sys
        .thunks
        .resolve(env.cpu, library, name)
        .map_err(|e| format!("binding {library}!{name}: {e}"))
}
