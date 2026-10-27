//! Wine Unix libraries of builtin modules, after Wine's
//! `get_builtin_unix_funcs`.
//!
//! A builtin DLL from Wine's `i386-windows` directory may have a Unix
//! library of the same name in `i386-unix`. The DLL asks for its function
//! table with `NtQueryVirtualMemory(MemoryWineUnixFuncs)` and then calls
//! through `__wine_unix_call_dispatcher` with that table as the handle. The
//! tables point to guest ELF code or registered host thunks. This module
//! records the table belonging to each image and uses one guest call path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

use crate::status::{self, Status};

pub const MEMORY_WINE_UNIX_FUNCS: u32 = 1000;
pub const MEMORY_WINE_UNIX_WOW64_FUNCS: u32 = 1001;

const DLL_NOT_FOUND: Status = 0xc000_0135;

/// What the loader does once it has the table, or failed to load.
pub type Loaded = Box<dyn FnOnce(&mut Env<'_>, Result<u32, Status>) -> Outcome>;

/// Loads the Unix library at a host path, at most once, and passes on the
/// guest address of its `__wine_unix_call_funcs`. The crate that loads
/// ELF code installs it.
#[derive(Clone, Copy)]
pub struct Loader(pub fn(&mut Env<'_>, &Path, Loaded) -> Outcome);

/// The Unix library of each mapped image that has one, by image base.
#[derive(Default)]
struct Libraries(HashMap<u32, Library>);

#[derive(Clone)]
enum Library {
    Guest(PathBuf),
    Host(u32),
}

/// The file name of the Unix library each guest table belongs to.
#[derive(Clone, Debug, Default)]
pub struct Names(pub HashMap<u32, String>);

/// Guest tables of host thunks, taking precedence over builtin ELF libraries.
#[derive(Default)]
pub struct HostTables(pub HashMap<String, u32>);

/// Records the Unix library next to the image file `dll`, mapped at `base`.
pub fn note_image(env: &mut Env<'_>, base: u32, dll: Option<PathBuf>) {
    let library = dll.as_deref().and_then(|dll| {
        let host = dll
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| {
                env.ext
                    .try_get::<HostTables>()?
                    .0
                    .get(&name.to_ascii_lowercase())
                    .copied()
                    .map(Library::Host)
            });
        host.or_else(|| library_of(dll).map(Library::Guest))
    });
    let libraries = env.ext.get_or_default::<Libraries>();
    match library {
        Some(library) => libraries.0.insert(base, library),
        None => libraries.0.remove(&base),
    };
}

/// Forgets the image at `base`, which was unmapped.
pub fn forget_image(env: &mut Env<'_>, base: u32) {
    env.ext.get_or_default::<Libraries>().0.remove(&base);
}

/// `<install>/i386-unix/<name>.so` for `<install>/i386-windows/<name>.dll`.
fn library_of(dll: &Path) -> Option<PathBuf> {
    let directory = dll.parent()?;
    if directory.file_name()? != "i386-windows" {
        return None;
    }
    let library = directory
        .parent()?
        .join("i386-unix")
        .join(Path::new(dll.file_stem()?).with_extension("so"));
    library.is_file().then_some(library)
}

/// NtQueryVirtualMemory(process, module, MemoryWineUnixFuncs, *handle,
/// length, *returned)
pub fn query(env: &mut Env<'_>, module: u32, class: u32, buffer: u32, length: u32) -> Outcome {
    if length != 8 {
        return Outcome::Return(status::INFO_LENGTH_MISMATCH);
    }
    // i386 processes have no WoW64 tables.
    if class == MEMORY_WINE_UNIX_WOW64_FUNCS {
        return Outcome::Return(DLL_NOT_FOUND);
    }
    let library = env
        .ext
        .get_or_default::<Libraries>()
        .0
        .get(&module)
        .cloned();
    let name = match &library {
        Some(Library::Guest(path)) => path.file_name().map(|name| name.to_string_lossy().into()),
        _ => None,
    };
    let complete: Loaded = Box::new(move |env, funcs| match funcs {
        Ok(funcs) => match env.cpu.write(buffer, &u64::from(funcs).to_le_bytes()) {
            Ok(()) => {
                if let Some(name) = name {
                    env.ext.get_or_default::<Names>().0.insert(funcs, name);
                }
                Outcome::Return(status::SUCCESS)
            }
            Err(_) => Outcome::Return(status::ACCESS_VIOLATION),
        },
        Err(status) => Outcome::Return(status),
    });
    match library {
        Some(Library::Host(table)) => complete(env, Ok(table)),
        Some(Library::Guest(path)) => match env.ext.try_get::<Loader>() {
            Some(&Loader(load)) => load(env, &path, complete),
            None => complete(env, Err(DLL_NOT_FOUND)),
        },
        None => complete(env, Err(DLL_NOT_FOUND)),
    }
}

/// Calls entry `code` of the table `funcs` with `args`, as Wine's
/// `__wine_unix_call` does.
pub fn call(env: &mut Env<'_>, funcs: u32, code: u32, args: u32) -> Outcome {
    let Ok(entry) = env.cpu.read_u32(funcs.wrapping_add(code.wrapping_mul(4))) else {
        return Outcome::Return(status::ACCESS_VIOLATION);
    };
    Outcome::call_aligned(env, entry, [args], |_, returned| {
        Outcome::Return(returned.value())
    })
}
