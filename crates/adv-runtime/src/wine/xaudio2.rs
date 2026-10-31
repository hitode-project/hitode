//! Wine DLL installation and guest calls for the native XAudio2 front.
use adv_abi::{
    Registry,
    dll::{Export, ModuleImage},
};
use adv_cpu::backend::Backend;
use adv_dx::xaudio2::{MODULES, Then};
use adv_machine::{Env, Machine, Outcome};
use adv_nt::process::{module_base, module_export};
use std::path::Path;
const ORIGINALS: [&str; 3] = [
    "hitode_xaudio2_7.dll",
    "hitode_xaudio2_8.dll",
    "hitode_xaudio2_9.dll",
];
struct Originals([u32; 3]);

pub(super) fn activate<B: Backend>(
    machine: &mut Machine<B>,
    main: adv_machine::CtxId,
) -> Result<(), String> {
    machine.visit_context(Some(main), |env| {
        let key = r"\Registry\Machine\Software\Classes\CLSID\{DB05EA35-0329-4D4B-A53A-6DEAD03D3852}\InprocServer32";
        adv_nt::registry::set_volatile_string(env, key, "", MODULES[0])
            .and_then(|()| adv_nt::registry::set_volatile_string(env, key, "ThreadingModel", "Both"))
            .map_err(|error| format!("XAudio2 debug class: {error:?}"))
    })?
}
pub(super) fn register(registry: &mut Registry) {
    adv_dx::xaudio2::register(registry);
}
pub(super) fn install<B: Backend>(machine: &mut Machine<B>, drive: &Path) -> Result<(), String> {
    adv_dx::xaudio2::install(
        machine,
        adv_dx::xaudio2::Host {
            allocator: super::allocator(),
            original,
            export: export_address,
        },
    )?;
    machine.ext.insert(Originals([0; 3]));
    for (version, module) in MODULES.into_iter().enumerate() {
        let path = drive.join("windows/system32").join(module);
        let original = path.with_file_name(ORIGINALS[version]);
        let source = std::fs::canonicalize(if path.is_symlink() { &path } else { &original })
            .map_err(|e| e.to_string())?;
        super::session::link(&source, &original)?;
        let pe = adv_pe::Pe::parse(&std::fs::read(source).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let names = pe.exports().map_err(|e| e.to_string())?;
        let exports: Vec<_> = names
            .iter()
            .filter_map(|e| {
                e.name.as_deref().map(|name| Export {
                    name,
                    ordinal: e.ordinal,
                })
            })
            .collect();
        let built = ModuleImage::build(machine, module, &exports, &[])?;
        adv_unix::builtin::install(drive, &built).map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn alloc(env: &mut Env<'_>, size: u32) -> Result<u32, String> {
    (super::allocator().allocate)(env, size).ok_or_else(|| "guest allocation failed".into())
}
fn free(env: &mut Env<'_>, at: u32) {
    (super::allocator().free)(env, at);
}
fn finish(result: Result<u32, String>) -> Outcome {
    match result {
        Ok(value) => Outcome::Return(value),
        Err(error) => Outcome::Fail(error),
    }
}
fn kernel(env: &Env<'_>, name: &str) -> Result<u32, String> {
    export_address(env, "kernel32.dll", name)
}
fn export_address(env: &Env<'_>, module: &str, name: &str) -> Result<u32, String> {
    module_base(env.cpu, env.sys.peb, module)
        .and_then(|base| module_export(env.cpu, base, name))
        .or_else(|| {
            let target = match module {
                "kernel32.dll" => "kernelbase.dll",
                "ole32.dll" => "combase.dll",
                _ => return None,
            };
            module_base(env.cpu, env.sys.peb, target)
                .and_then(|base| module_export(env.cpu, base, name))
        })
        .ok_or_else(|| format!("{module} exports no {name}"))
}
fn original(
    env: &mut Env<'_>,
    version: usize,
    name: &'static str,
    args: Vec<u32>,
    then: Then,
) -> Outcome {
    let base = env.ext.get::<Originals>().0[version];
    if base != 0 {
        return original_at(env, base, name, args, then);
    }
    let wide: Vec<u8> = ORIGINALS[version]
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let prepared = (|| {
        let at = alloc(env, wide.len() as u32)?;
        env.cpu.write(at, &wide).map_err(|e| e.to_string())?;
        Ok((at, kernel(env, "LoadLibraryW")?))
    })();
    let (at, target) = match prepared {
        Ok(value) => value,
        Err(error) => return finish(Err(error)),
    };
    Outcome::call(target, vec![at], move |env, result| {
        free(env, at);
        let base = result.value();
        if base == 0 {
            return Outcome::Return(0x8007_007e);
        }
        env.ext.get_mut::<Originals>().0[version] = base;
        original_at(env, base, name, args, then)
    })
}
fn original_at(
    env: &mut Env<'_>,
    base: u32,
    name: &'static str,
    args: Vec<u32>,
    then: Then,
) -> Outcome {
    match module_export(env.cpu, base, name) {
        Some(target) => Outcome::call(target, args, move |env, result| then(env, result.value())),
        None => Outcome::Return(0x8007_007f),
    }
}
