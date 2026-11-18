//! Live hardware key state is independent of the guest message backlog.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

type PressedListener = dyn Fn(bool) + Send + Sync;

pub struct KeyState {
    // Value and generation share one atomic snapshot. Separate atomics could
    // attribute an old GetAsyncKeyState value to a new physical release.
    keys: [AtomicU64; 256],
    pressed: [AtomicU64; 4],
    write: Mutex<Vec<Weak<PressedListener>>>,
}
impl Default for KeyState {
    fn default() -> Self {
        Self {
            keys: std::array::from_fn(|_| AtomicU64::new(0)),
            pressed: [const { AtomicU64::new(0) }; 4],
            write: Mutex::new(Vec::new()),
        }
    }
}
impl KeyState {
    /// Publish the initial held state and subsequent transitions while serializing writes.
    /// Presses are published before they become visible and releases afterwards.
    pub fn watch_pressed(&self, listener: &Arc<PressedListener>) {
        let mut listeners = self.write.lock().unwrap_or_else(|p| p.into_inner());
        listener(self.any_pressed());
        listeners.retain(|listener| listener.strong_count() > 0);
        listeners.push(Arc::downgrade(listener));
    }

    pub fn unwatch_pressed(&self, listener: &Arc<PressedListener>) {
        let listener = Arc::downgrade(listener);
        self.write
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|registered| !registered.ptr_eq(&listener));
    }

    fn publish_pressed(listeners: &mut Vec<Weak<PressedListener>>, pressed: bool) {
        listeners.retain(|listener| {
            if let Some(listener) = listener.upgrade() {
                listener(pressed);
                true
            } else {
                false
            }
        });
    }

    pub fn set(&self, key: u32, pressed: bool) {
        let Some(value) = self.keys.get(key as usize) else {
            return;
        };
        let mut listeners = self.write.lock().unwrap_or_else(|p| p.into_inner());
        let was_pressed = !listeners.is_empty() && self.any_pressed();
        if pressed && !was_pressed && !listeners.is_empty() {
            Self::publish_pressed(&mut listeners, true);
        }
        let _ = value.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            let toggle = if pressed && old & 0x80 == 0 && matches!(key, 0x14 | 0x90 | 0x91) {
                old ^ 1
            } else {
                old
            };
            let generation = (old & !0xff).wrapping_add(0x100);
            Some(generation | (toggle & 1) | if pressed { 0x80 } else { 0 })
        });
        let word = &self.pressed[(key >> 6) as usize];
        let bit = 1u64 << (key & 63);
        if pressed {
            word.fetch_or(bit, Ordering::Release);
        } else {
            word.fetch_and(!bit, Ordering::Relaxed);
            if was_pressed && !self.any_pressed() {
                Self::publish_pressed(&mut listeners, false);
            }
        }
    }
    pub fn get(&self, key: u32) -> u8 {
        self.observation(key) as u8
    }
    /// Opaque generation together with the key value in the low byte.
    pub fn observation(&self, key: u32) -> u64 {
        self.keys
            .get(key as usize)
            .map_or(0, |key| key.load(Ordering::Relaxed))
    }
    pub fn release_all(&self) {
        let mut listeners = self.write.lock().unwrap_or_else(|p| p.into_inner());
        let was_pressed = !listeners.is_empty() && self.any_pressed();
        for (i, key) in self.keys.iter().enumerate() {
            let _ = key.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                (old & 0x80 != 0).then(|| (old & !0xff).wrapping_add(0x100) | (old & 1))
            });
            self.pressed[i >> 6].fetch_and(!(1u64 << (i & 63)), Ordering::Relaxed);
        }
        if was_pressed {
            Self::publish_pressed(&mut listeners, false);
        }
    }
    /// Some key or mouse button is down.
    pub fn any_pressed(&self) -> bool {
        self.pressed
            .iter()
            .any(|word| word.load(Ordering::Relaxed) != 0)
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
        assert!(!keys.any_pressed());
        let initial = keys.observation(17);
        keys.set(17, true);
        assert!(keys.any_pressed());
        let down = keys.observation(17);
        keys.set(17, false);
        assert!(!keys.any_pressed());
        let up = keys.observation(17);
        assert_eq!(initial as u8, up as u8);
        assert_ne!(initial, up);
        assert_eq!(down as u8, 0x80);
        keys.set(17, false);
        assert_ne!(keys.observation(17), up);
        keys.set(0x14, true);
        keys.set(0x14, true);
        assert_eq!(keys.get(0x14), 0x81);
        assert!(keys.any_pressed());
        keys.release_all();
        assert!(!keys.any_pressed());
        assert_eq!(keys.get(0x14), 1);
        assert_eq!(keys.snapshot()[17], 0);
        assert_eq!(keys.observation(256), 0);
        for key in [0u32, 64, 128, 255] {
            keys.set(key, true);
            assert!(keys.any_pressed());
            keys.set(key, false);
            assert!(!keys.any_pressed());
        }
    }
}
