//! DirectSound on the Wine userland: the built-in stack's front as a PE image
//! in place of Wine's dsound.dll, mixing on the host over the session's
//! audio stream, so no guest thread's scheduling paces playback.
use std::path::Path;

use adv_abi::dll::{Export, ModuleImage};
use adv_abi::{Conv, Registry};
use adv_cpu::backend::Backend;
use adv_machine::{Machine, Outcome};

pub(super) const MODULE: &str = "dsound.dll";

/// Wine's dsound.spec exports at their ordinals; games import some by ordinal.
const EXPORTS: [(&str, u16); 12] = [
    ("DirectSoundCreate", 1),
    ("DirectSoundEnumerateA", 2),
    ("DirectSoundEnumerateW", 3),
    ("DllCanUnloadNow", 4),
    ("DllGetClassObject", 5),
    ("DirectSoundCaptureCreate", 6),
    ("DirectSoundCaptureEnumerateA", 7),
    ("DirectSoundCaptureEnumerateW", 8),
    ("GetDeviceID", 9),
    ("DirectSoundFullDuplexCreate", 10),
    ("DirectSoundCreate8", 11),
    ("DirectSoundCaptureCreate8", 12),
];

pub(super) fn register(registry: &mut Registry) {
    adv_dx::dsound::register(registry);
    // The built-in stack has no capture devices: enumeration lists nothing.
    for name in [
        "DirectSoundCaptureEnumerateA",
        "DirectSoundCaptureEnumerateW",
    ] {
        registry.add(MODULE, name, Conv::Stdcall { argc: 2 }, |_| {
            Outcome::Return(0)
        });
    }
}

/// Install the front over the session's audio stream and put its image in
/// drive C: at `drive`, in place of Wine's.
pub(super) fn install<B: Backend>(
    machine: &mut Machine<B>,
    drive: &Path,
    allocator: adv_dx::interop::GuestAllocator,
) -> Result<(), String> {
    if machine.ext.try_get::<adv_device::AudioControl>().is_none() {
        let real = !machine.sys.clock.is_virtual();
        machine.ext.insert(adv_device::AudioControl::new(real));
    }
    let audio = machine.ext.get::<adv_device::AudioControl>().clone();
    let host = adv_dx::dsound::Host {
        allocator: Some(allocator),
        events: Some(adv_dx::dsound::Events {
            // Wine's dsound keeps any handle; a bad one fails when signalled.
            valid: |_, handle| handle != 0,
            // As Wine's dsound, a notification that cannot be set is skipped.
            signal: |env, handle| match adv_nt::sync::set_audio_event(env, handle) {
                Ok(()) | Err(adv_nt::status::Error::Status(_)) => Ok(()),
                Err(error) => Err(format!("{error:?}")),
            },
        }),
    };
    adv_dx::dsound::install_on_host(machine, audio, host)?;
    machine.ext.get_mut::<adv_dx::dsound::Sound>().enabled = true;
    let exports: Vec<_> = EXPORTS
        .into_iter()
        .map(|(name, ordinal)| Export { name, ordinal })
        .collect();
    let built = ModuleImage::build(machine, MODULE, &exports, &[])?;
    adv_unix::builtin::install(drive, &built).map_err(|e| e.to_string())?;
    Ok(())
}
