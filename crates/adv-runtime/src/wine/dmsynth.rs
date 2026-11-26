//! DirectMusic software synthesizer on the Wine userland: host-module PE image
//! for `dmsynth.dll` over `adv_dx::dmsynth`, synthesizing on the host with
//! `adv-device::midi::Synth` and the session's shared PCM mixer.
use std::path::Path;

use adv_abi::Registry;
use adv_abi::dll::{Export, ModuleImage};
use adv_cpu::backend::Backend;
use adv_machine::Machine;

const EXPORTS: [(&str, u16); 4] = [
    ("DllCanUnloadNow", 1),
    ("DllGetClassObject", 2),
    ("DllRegisterServer", 3),
    ("DllUnregisterServer", 4),
];

pub(super) fn register(registry: &mut Registry) {
    adv_dx::dmsynth::register(registry);
}

pub(super) fn install<B: Backend>(
    machine: &mut Machine<B>,
    drive: &Path,
    allocator: adv_dx::interop::GuestAllocator,
) -> Result<(), String> {
    adv_dx::dmsynth::install(machine, allocator)?;
    let exports: Vec<_> = EXPORTS
        .into_iter()
        .map(|(name, ordinal)| Export { name, ordinal })
        .collect();
    let built = ModuleImage::build(machine, adv_dx::dmsynth::MODULE, &exports, &[])?;
    adv_unix::builtin::install(drive, &built).map_err(|e| e.to_string())?;
    Ok(())
}

/// Ensure `C:\windows\system32\drivers\gm.dls` exists so Wine's `dmloader.dll`
/// resolves `GUID_DefaultGMCollection`. A user-supplied file stays untouched.
pub(super) fn ensure_gm_dls(drive: &Path) -> Result<(), String> {
    let drivers = drive.join("windows/system32/drivers");
    let path = drivers.join("gm.dls");
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&drivers).map_err(|e| format!("{}: {e}", drivers.display()))?;
    std::fs::write(&path, adv_dx::dmsynth::default_gm_dls())
        .map_err(|e| format!("{}: {e}", path.display()))
}
