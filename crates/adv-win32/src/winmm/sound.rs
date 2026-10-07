//! Legacy named-sound playback has no active channel until a provider exists.
//! Stopping that empty channel never affects waveOut, DirectSound or movie audio.
use adv_abi::{AnsiStr, Registry, WideStr, warn_unavailable, win32};
use adv_machine::Env;

fn sound(env: &mut Env<'_>, pointer: u32, api: &'static str) -> i32 {
    if pointer == 0 {
        return 1;
    }
    warn_unavailable(
        env,
        api,
        "legacy named/memory sound playback is unavailable",
    );
    0
}

#[win32(winmm, name = "sndPlaySoundA", level = safe_fail)]
fn play_a(env: &mut Env<'_>, name: AnsiStr, _flags: u32) -> i32 {
    sound(env, name.0.addr(), "winmm.dll!sndPlaySoundA")
}

#[win32(winmm, name = "sndPlaySoundW", level = safe_fail)]
fn play_w(env: &mut Env<'_>, name: WideStr, _flags: u32) -> i32 {
    sound(env, name.0.addr(), "winmm.dll!sndPlaySoundW")
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(play_a_ABI);
    registry.register(play_w_ABI);
}
