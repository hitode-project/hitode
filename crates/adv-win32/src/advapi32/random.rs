//! RtlGenRandom (SystemFunction036), which the CRT's rand_s loads by name.
//! Bytes come from a per-process SplitMix64 stream seeded with host entropy;
//! guests only need unpredictable values, not a cryptographic provider.
//! API contract: https://learn.microsoft.com/windows/win32/api/ntsecapi/nf-ntsecapi-rtlgenrandom
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;
use std::hash::{BuildHasher, RandomState};

#[cfg(test)]
mod tests;

#[derive(Default)]
struct Stream(Option<u64>);

pub(super) fn register(registry: &mut Registry) {
    registry.register(generate_ABI);
}

#[win32(advapi32, name = "SystemFunction036", level = implement)]
fn generate(env: &mut Env<'_>, buffer: GuestPtr<u8>, length: u32) -> u32 {
    if check_range(env.cpu, buffer.addr(), length, Prot::WRITE).is_err() {
        return 0;
    }
    let stream = env.ext.get_or_default::<Stream>();
    let state = stream
        .0
        .get_or_insert_with(|| RandomState::new().hash_one(0x5eed_u64));
    let mut bytes = Vec::with_capacity(length as usize + 8);
    while bytes.len() < length as usize {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        bytes.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    bytes.truncate(length as usize);
    u32::from(env.cpu.write(buffer.addr(), &bytes).is_ok())
}
