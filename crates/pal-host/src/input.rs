//! Live hardware key state is independent of the guest message backlog.
use std::sync::atomic::{AtomicU64, Ordering};
pub struct KeyState {
    // Value and generation share one atomic snapshot. Separate atomics could
    // attribute an old GetAsyncKeyState value to a new physical release.
    keys: [AtomicU64; 256],
}
impl Default for KeyState {
    fn default() -> Self {
        Self { keys: std::array::from_fn(|_| AtomicU64::new(0)) }
    }
}
impl KeyState {
    pub fn set(&self, key: u32, pressed: bool) {
        let Some(value) = self.keys.get(key as usize) else {
            return;
        };
        let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            let toggle = if pressed && old & 0x80 == 0 && matches!(key, 0x14 | 0x90 | 0x91) { old ^ 1 } else { old };
            let generation = (old & !0xff).wrapping_add(0x100);
            Some(generation | (toggle & 1) | if pressed { 0x80 } else { 0 })
        });
    }
    pub fn get(&self, key: u32) -> u8 {
        self.observation(key) as u8
    }
    /// Opaque generation together with the key value in the low byte.
    pub fn observation(&self, key: u32) -> u64 {
        self.keys.get(key as usize).map_or(0, |key| key.load(Ordering::Relaxed))
    }
    pub fn release_all(&self) {
        for key in &self.keys {
            let _ = key.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                (old & 0x80 != 0).then(|| (old & !0xff).wrapping_add(0x100) | (old & 1))
            });
        }
    }
    pub fn snapshot(&self) -> [u8; 256] {
        std::array::from_fn(|key| self.get(key as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_values_have_distinct_coherent_generations() {
        let keys = KeyState::default();
        let initial = keys.observation(17);
        keys.set(17, true);
        let down = keys.observation(17);
        keys.set(17, false);
        let up = keys.observation(17);
        assert_eq!(initial as u8, up as u8);
        assert_ne!(initial, up);
        assert_eq!(down as u8, 0x80);
        keys.set(17, false);
        assert_ne!(keys.observation(17), up);
        keys.set(0x14, true);
        keys.set(0x14, true);
        assert_eq!(keys.get(0x14), 0x81);
        keys.release_all();
        assert_eq!(keys.get(0x14), 1);
        assert_eq!(keys.snapshot()[17], 0);
        assert_eq!(keys.observation(256), 0);
    }
}
