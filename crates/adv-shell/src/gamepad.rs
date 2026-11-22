//! Platform-neutral external gamepad mapping state machine.
//!
//! Translates physical-position controller buttons and analog sticks into
//! Win32 keyboard, mouse, wheel, and launcher system actions. Supports
//! multiple simultaneous controllers, per-device confirm/cancel swap, latch
//! toggles, and demand-only stick ticking (zero wakeups while sticks rest).

use std::collections::{BTreeMap, HashMap, HashSet};

/// Physical-position controller button identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GamepadButton {
    South,
    East,
    West,
    North,
    L1,
    R1,
    L2,
    R2,
    L3,
    R3,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Start,
    Select,
}

impl GamepadButton {
    pub const ALL: [Self; 16] = [
        Self::South,
        Self::East,
        Self::West,
        Self::North,
        Self::L1,
        Self::R1,
        Self::L2,
        Self::R2,
        Self::L3,
        Self::R3,
        Self::DpadUp,
        Self::DpadDown,
        Self::DpadLeft,
        Self::DpadRight,
        Self::Start,
        Self::Select,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::South => "south",
            Self::East => "east",
            Self::West => "west",
            Self::North => "north",
            Self::L1 => "l1",
            Self::R1 => "r1",
            Self::L2 => "l2",
            Self::R2 => "r2",
            Self::L3 => "l3",
            Self::R3 => "r3",
            Self::DpadUp => "dpadUp",
            Self::DpadDown => "dpadDown",
            Self::DpadLeft => "dpadLeft",
            Self::DpadRight => "dpadRight",
            Self::Start => "start",
            Self::Select => "select",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "south" => Some(Self::South),
            "east" => Some(Self::East),
            "west" => Some(Self::West),
            "north" => Some(Self::North),
            "l1" => Some(Self::L1),
            "r1" => Some(Self::R1),
            "l2" => Some(Self::L2),
            "r2" => Some(Self::R2),
            "l3" => Some(Self::L3),
            "r3" => Some(Self::R3),
            "dpadUp" => Some(Self::DpadUp),
            "dpadDown" => Some(Self::DpadDown),
            "dpadLeft" => Some(Self::DpadLeft),
            "dpadRight" => Some(Self::DpadRight),
            "start" => Some(Self::Start),
            "select" => Some(Self::Select),
            _ => None,
        }
    }
}

/// Controller glyph family detected from hardware metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamepadStyle {
    Xbox,
    PlayStation,
    Switch,
}

impl GamepadStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xbox => "xbox",
            Self::PlayStation => "ps",
            Self::Switch => "switch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Middle => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemAction {
    Menu,
    Hide,
    Kbd,
}

impl SystemAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::Hide => "hide",
            Self::Kbd => "kbd",
        }
    }
}

/// Target action bound to a controller button, matching `key-layouts.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyBinding {
    Vk(u32),
    Mouse(MouseButton),
    Wheel(i32),
    Sys(SystemAction),
}

impl KeyBinding {
    pub fn can_latch(self) -> bool {
        matches!(self, Self::Vk(_) | Self::Mouse(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonEntry {
    pub binding: KeyBinding,
    pub latch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StickRole {
    Cursor,
    Dpad,
    Wheel,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpadBind {
    Arrows,
    Wasd,
    Numpad,
}

impl DpadBind {
    fn directions(self) -> [u32; 4] {
        // [up, down, left, right]
        match self {
            Self::Arrows => [0x26, 0x28, 0x25, 0x27],
            Self::Wasd => [0x57, 0x53, 0x41, 0x44],
            Self::Numpad => [0x68, 0x62, 0x64, 0x66],
        }
    }
}

/// Active mapping profile for a controller.
#[derive(Clone, Debug, PartialEq)]
pub struct GamepadProfile {
    pub left_stick: StickRole,
    pub right_stick: StickRole,
    pub stick_dpad_bind: DpadBind,
    pub cursor_speed: f32,
    pub bindings: BTreeMap<GamepadButton, ButtonEntry>,
}

impl Default for GamepadProfile {
    fn default() -> Self {
        let mut bindings = BTreeMap::new();
        let mut set = |btn, binding, latch| {
            bindings.insert(btn, ButtonEntry { binding, latch });
        };
        set(
            GamepadButton::South,
            KeyBinding::Mouse(MouseButton::Left),
            false,
        );
        set(
            GamepadButton::East,
            KeyBinding::Mouse(MouseButton::Right),
            false,
        );
        set(GamepadButton::West, KeyBinding::Vk(0x20), false);
        set(GamepadButton::North, KeyBinding::Vk(0x0d), false);
        set(GamepadButton::L1, KeyBinding::Vk(0x1b), false);
        set(GamepadButton::R1, KeyBinding::Vk(0x11), true);
        set(GamepadButton::L2, KeyBinding::Wheel(1), false);
        set(GamepadButton::R2, KeyBinding::Wheel(-1), false);
        set(
            GamepadButton::R3,
            KeyBinding::Mouse(MouseButton::Middle),
            false,
        );
        set(GamepadButton::DpadUp, KeyBinding::Vk(0x26), false);
        set(GamepadButton::DpadDown, KeyBinding::Vk(0x28), false);
        set(GamepadButton::DpadLeft, KeyBinding::Vk(0x25), false);
        set(GamepadButton::DpadRight, KeyBinding::Vk(0x27), false);
        set(
            GamepadButton::Start,
            KeyBinding::Sys(SystemAction::Menu),
            false,
        );
        set(
            GamepadButton::Select,
            KeyBinding::Sys(SystemAction::Kbd),
            false,
        );
        Self {
            left_stick: StickRole::Cursor,
            right_stick: StickRole::Wheel,
            stick_dpad_bind: DpadBind::Arrows,
            cursor_speed: 1.0,
            bindings,
        }
    }
}

/// Resolved per-device effective configuration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceConfig {
    pub swap_confirm_cancel: bool,
    pub profile: GamepadProfile,
}

/// Output emitted by [`GamepadEngine`] for the host to dispatch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GamepadOutput {
    Key { vk: u32, down: bool },
    MouseButton { button: u8, down: bool },
    MouseMove { dx: f32, dy: f32 },
    Wheel { notches: i32 },
    System(SystemAction),
}

const STICK_DEADZONE: f32 = 0.20;
const WHEEL_DEADZONE: f32 = 0.28;
const DPAD_ON: f32 = 0.45;
const DPAD_OFF: f32 = 0.30;
/// Canvas pixels per second at full deflection and `cursor_speed == 1.0`.
const BASE_CURSOR_PX_PER_SEC: f32 = 580.0;
/// Max scroll notches per second at full stick deflection.
const MAX_WHEEL_NOTCHES_PER_SEC: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HeldOutput {
    Vk(u32),
    Mouse(MouseButton),
}

#[derive(Default)]
struct StickState {
    /// Raw normalized axes: +x right, +y down (screen coordinates).
    x: f32,
    y: f32,
    role: Option<StickRole>,
    cursor_speed: f32,
    wheel_accum: f32,
    wheel_sign: i8,
    /// Currently held dpad keys for [up, down, left, right].
    dpad_held: [Option<u32>; 4],
}

impl StickState {
    fn needs_tick(&self) -> bool {
        match self.role {
            Some(StickRole::Cursor) => (self.x * self.x + self.y * self.y).sqrt() > STICK_DEADZONE,
            Some(StickRole::Wheel) => self.y.abs() > WHEEL_DEADZONE,
            _ => false,
        }
    }
}

#[derive(Default)]
struct DeviceState {
    pressed: HashSet<GamepadButton>,
    /// Active held/latched outputs owned by each physical button.
    button_holds: HashMap<GamepadButton, HeldOutput>,
    latched: HashSet<GamepadButton>,
    left: StickState,
    right: StickState,
}

/// Multi-controller mapping engine.
#[derive(Default)]
pub struct GamepadEngine {
    devices: HashMap<String, DeviceState>,
    vk_refs: HashMap<u32, usize>,
    mouse_refs: [usize; 3],
}

impl GamepadEngine {
    pub fn new() -> Self {
        Self::default()
    }

    fn effective_button(button: GamepadButton, swap: bool) -> GamepadButton {
        if !swap {
            return button;
        }
        match button {
            GamepadButton::South => GamepadButton::East,
            GamepadButton::East => GamepadButton::South,
            other => other,
        }
    }

    fn acquire(&mut self, output: HeldOutput, out: &mut Vec<GamepadOutput>) {
        match output {
            HeldOutput::Vk(vk) => {
                let count = self.vk_refs.entry(vk).or_insert(0);
                *count += 1;
                if *count == 1 {
                    out.push(GamepadOutput::Key { vk, down: true });
                }
            }
            HeldOutput::Mouse(btn) => {
                let idx = btn.code() as usize;
                self.mouse_refs[idx] += 1;
                if self.mouse_refs[idx] == 1 {
                    out.push(GamepadOutput::MouseButton {
                        button: btn.code(),
                        down: true,
                    });
                }
            }
        }
    }

    fn release(&mut self, output: HeldOutput, out: &mut Vec<GamepadOutput>) {
        match output {
            HeldOutput::Vk(vk) => {
                if let Some(count) = self.vk_refs.get_mut(&vk) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.vk_refs.remove(&vk);
                        out.push(GamepadOutput::Key { vk, down: false });
                    }
                }
            }
            HeldOutput::Mouse(btn) => {
                let idx = btn.code() as usize;
                if self.mouse_refs[idx] > 0 {
                    self.mouse_refs[idx] -= 1;
                    if self.mouse_refs[idx] == 0 {
                        out.push(GamepadOutput::MouseButton {
                            button: btn.code(),
                            down: false,
                        });
                    }
                }
            }
        }
    }

    /// Processes a physical button press or release.
    pub fn on_button(
        &mut self,
        device_id: &str,
        button: GamepadButton,
        down: bool,
        config: &DeviceConfig,
    ) -> Vec<GamepadOutput> {
        let mut out = Vec::new();
        let state = self.devices.entry(device_id.to_string()).or_default();
        if down {
            if !state.pressed.insert(button) {
                return out;
            }
        } else if !state.pressed.remove(&button) {
            return out;
        }

        let slot = Self::effective_button(button, config.swap_confirm_cancel);
        let entry = config.profile.bindings.get(&slot).copied();

        if down {
            let Some(entry) = entry else {
                return out;
            };
            match entry.binding {
                KeyBinding::Vk(vk) => {
                    let held = HeldOutput::Vk(vk);
                    if entry.latch {
                        let was_latched = state.latched.remove(&button);
                        if was_latched {
                            if let Some(prev) = state.button_holds.remove(&button) {
                                self.release(prev, &mut out);
                            }
                        } else {
                            let prev = state.button_holds.insert(button, held);
                            state.latched.insert(button);
                            if let Some(prev) = prev {
                                self.release(prev, &mut out);
                            }
                            self.acquire(held, &mut out);
                        }
                    } else {
                        state.latched.remove(&button);
                        let prev = state.button_holds.insert(button, held);
                        if let Some(prev) = prev {
                            self.release(prev, &mut out);
                        }
                        self.acquire(held, &mut out);
                    }
                }
                KeyBinding::Mouse(btn) => {
                    let held = HeldOutput::Mouse(btn);
                    if entry.latch {
                        let was_latched = state.latched.remove(&button);
                        if was_latched {
                            if let Some(prev) = state.button_holds.remove(&button) {
                                self.release(prev, &mut out);
                            }
                        } else {
                            let prev = state.button_holds.insert(button, held);
                            state.latched.insert(button);
                            if let Some(prev) = prev {
                                self.release(prev, &mut out);
                            }
                            self.acquire(held, &mut out);
                        }
                    } else {
                        state.latched.remove(&button);
                        let prev = state.button_holds.insert(button, held);
                        if let Some(prev) = prev {
                            self.release(prev, &mut out);
                        }
                        self.acquire(held, &mut out);
                    }
                }
                KeyBinding::Wheel(notches) => {
                    if notches != 0 {
                        out.push(GamepadOutput::Wheel { notches });
                    }
                }
                KeyBinding::Sys(action) => {
                    out.push(GamepadOutput::System(action));
                }
            }
        } else {
            // Releasing a latched button leaves its hold active until the next press.
            if !state.latched.contains(&button)
                && let Some(prev) = state.button_holds.remove(&button)
            {
                self.release(prev, &mut out);
            }
        }
        out
    }

    /// Processes a stick axis update (`x`: +right, `y`: +down, normalized to `[-1, 1]`).
    pub fn on_stick(
        &mut self,
        device_id: &str,
        right_stick: bool,
        x: f32,
        y: f32,
        config: &DeviceConfig,
    ) -> Vec<GamepadOutput> {
        let mut out = Vec::new();
        let x = if x.is_finite() {
            x.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let y = if y.is_finite() {
            y.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let role = if right_stick {
            config.profile.right_stick
        } else {
            config.profile.left_stick
        };
        let speed = config.profile.cursor_speed.clamp(0.5, 2.0);
        let dpad_keys = config.profile.stick_dpad_bind.directions();

        let (to_release, to_acquire, wheel_now) = {
            let state = self.devices.entry(device_id.to_string()).or_default();
            let stick = if right_stick {
                &mut state.right
            } else {
                &mut state.left
            };
            stick.x = x;
            stick.y = y;
            stick.role = Some(role);
            stick.cursor_speed = speed;

            let mut rel = Vec::new();
            let mut acq = Vec::new();
            let mut wheel_now = 0i32;

            if role == StickRole::Dpad {
                // Up (-y), Down (+y), Left (-x), Right (+x)
                let axes = [-y, y, -x, x];
                for i in 0..4 {
                    let wanted = if stick.dpad_held[i].is_some() {
                        axes[i] > DPAD_OFF
                    } else {
                        axes[i] > DPAD_ON
                    };
                    let target_vk = dpad_keys[i];
                    match (stick.dpad_held[i], wanted) {
                        (Some(prev), false) => {
                            stick.dpad_held[i] = None;
                            rel.push(HeldOutput::Vk(prev));
                        }
                        (Some(prev), true) if prev != target_vk => {
                            stick.dpad_held[i] = Some(target_vk);
                            rel.push(HeldOutput::Vk(prev));
                            acq.push(HeldOutput::Vk(target_vk));
                        }
                        (None, true) => {
                            stick.dpad_held[i] = Some(target_vk);
                            acq.push(HeldOutput::Vk(target_vk));
                        }
                        _ => {}
                    }
                }
            } else {
                for slot in &mut stick.dpad_held {
                    if let Some(prev) = slot.take() {
                        rel.push(HeldOutput::Vk(prev));
                    }
                }
            }

            if role == StickRole::Wheel {
                let sign = if y < -WHEEL_DEADZONE {
                    1
                } else if y > WHEEL_DEADZONE {
                    -1
                } else {
                    0
                };
                if sign != stick.wheel_sign {
                    stick.wheel_sign = sign;
                    stick.wheel_accum = 0.0;
                    if sign != 0 {
                        wheel_now = i32::from(sign);
                    }
                }
            } else {
                stick.wheel_sign = 0;
                stick.wheel_accum = 0.0;
            }

            (rel, acq, wheel_now)
        };

        for r in to_release {
            self.release(r, &mut out);
        }
        for a in to_acquire {
            self.acquire(a, &mut out);
        }
        if wheel_now != 0 {
            out.push(GamepadOutput::Wheel { notches: wheel_now });
        }
        out
    }

    /// True only while at least one stick needs continuous cursor/wheel ticks.
    pub fn needs_tick(&self) -> bool {
        self.devices
            .values()
            .any(|d| d.left.needs_tick() || d.right.needs_tick())
    }

    /// Advances continuous stick roles (`Cursor`, `Wheel`) by `dt_secs`.
    pub fn tick(&mut self, dt_secs: f32) -> Vec<GamepadOutput> {
        let mut out = Vec::new();
        if !dt_secs.is_finite() || dt_secs <= 0.0 {
            return out;
        }
        let dt = dt_secs.min(0.1);
        let mut total_dx = 0.0f32;
        let mut total_dy = 0.0f32;
        let mut total_notches = 0i32;

        for device in self.devices.values_mut() {
            for stick in [&mut device.left, &mut device.right] {
                match stick.role {
                    Some(StickRole::Cursor) => {
                        let mag = (stick.x * stick.x + stick.y * stick.y).sqrt();
                        if mag > STICK_DEADZONE {
                            let norm = ((mag - STICK_DEADZONE) / (1.0 - STICK_DEADZONE)).min(1.0);
                            // Gentle quadratic curve: precise near center, brisk at full tilt.
                            let curved = norm * norm;
                            let scale =
                                BASE_CURSOR_PX_PER_SEC * stick.cursor_speed * dt * curved / mag;
                            total_dx += stick.x * scale;
                            total_dy += stick.y * scale;
                        }
                    }
                    Some(StickRole::Wheel) => {
                        let abs_y = stick.y.abs();
                        if abs_y > WHEEL_DEADZONE {
                            let norm = ((abs_y - WHEEL_DEADZONE) / (1.0 - WHEEL_DEADZONE)).min(1.0);
                            let rate = 2.0 + norm * (MAX_WHEEL_NOTCHES_PER_SEC - 2.0);
                            stick.wheel_accum += rate * dt;
                            let steps = stick.wheel_accum.trunc() as i32;
                            if steps > 0 {
                                stick.wheel_accum -= steps as f32;
                                let dir = if stick.y < 0.0 { 1 } else { -1 };
                                total_notches += dir * steps;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if total_dx.abs() > 0.01 || total_dy.abs() > 0.01 {
            out.push(GamepadOutput::MouseMove {
                dx: total_dx,
                dy: total_dy,
            });
        }
        if total_notches != 0 {
            out.push(GamepadOutput::Wheel {
                notches: total_notches,
            });
        }
        out
    }

    /// Releases every output owned by a disconnected device.
    pub fn disconnect_device(&mut self, device_id: &str) -> Vec<GamepadOutput> {
        let mut out = Vec::new();
        let Some(dev) = self.devices.remove(device_id) else {
            return out;
        };
        for held in dev.button_holds.into_values() {
            self.release(held, &mut out);
        }
        for stick in [dev.left, dev.right] {
            for vk in stick.dpad_held.into_iter().flatten() {
                self.release(HeldOutput::Vk(vk), &mut out);
            }
        }
        out
    }

    /// Releases all held and latched keys/mouse buttons (e.g. on pause or profile switch).
    pub fn reset_all(&mut self) -> Vec<GamepadOutput> {
        let mut out = Vec::new();
        let ids: Vec<String> = self.devices.keys().cloned().collect();
        for id in ids {
            out.extend(self.disconnect_device(&id));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_cancel_swap_and_latch_toggle_work() {
        let mut engine = GamepadEngine::new();
        let mut cfg = DeviceConfig::default();

        // Default: South -> Left click, R1 -> Ctrl (latch).
        assert_eq!(
            engine.on_button("p1", GamepadButton::South, true, &cfg),
            vec![GamepadOutput::MouseButton {
                button: 0,
                down: true
            }]
        );
        assert_eq!(
            engine.on_button("p1", GamepadButton::South, false, &cfg),
            vec![GamepadOutput::MouseButton {
                button: 0,
                down: false
            }]
        );

        // Switch-style swap: East -> Left click, South -> Right click.
        cfg.swap_confirm_cancel = true;
        assert_eq!(
            engine.on_button("p1", GamepadButton::East, true, &cfg),
            vec![GamepadOutput::MouseButton {
                button: 0,
                down: true
            }]
        );
        assert_eq!(
            engine.on_button("p1", GamepadButton::East, false, &cfg),
            vec![GamepadOutput::MouseButton {
                button: 0,
                down: false
            }]
        );
        assert_eq!(
            engine.on_button("p1", GamepadButton::South, true, &cfg),
            vec![GamepadOutput::MouseButton {
                button: 1,
                down: true
            }]
        );
        assert_eq!(
            engine.on_button("p1", GamepadButton::South, false, &cfg),
            vec![GamepadOutput::MouseButton {
                button: 1,
                down: false
            }]
        );

        // R1 Ctrl latch: press holds, release stays held, second press releases.
        assert_eq!(
            engine.on_button("p1", GamepadButton::R1, true, &cfg),
            vec![GamepadOutput::Key {
                vk: 0x11,
                down: true
            }]
        );
        assert!(
            engine
                .on_button("p1", GamepadButton::R1, false, &cfg)
                .is_empty()
        );
        assert_eq!(
            engine.on_button("p1", GamepadButton::R1, true, &cfg),
            vec![GamepadOutput::Key {
                vk: 0x11,
                down: false
            }]
        );
        assert!(
            engine
                .on_button("p1", GamepadButton::R1, false, &cfg)
                .is_empty()
        );
    }

    #[test]
    fn multi_controller_ref_counts_and_disconnect_cleanup() {
        let mut engine = GamepadEngine::new();
        let cfg = DeviceConfig::default();

        // Both controllers hold West (Space = 0x20).
        assert_eq!(
            engine.on_button("xbox", GamepadButton::West, true, &cfg),
            vec![GamepadOutput::Key {
                vk: 0x20,
                down: true
            }]
        );
        assert!(
            engine
                .on_button("switch", GamepadButton::West, true, &cfg)
                .is_empty()
        );

        // Releasing Switch keeps Space held because Xbox still holds it.
        assert!(
            engine
                .on_button("switch", GamepadButton::West, false, &cfg)
                .is_empty()
        );

        // Disconnecting Xbox releases Space cleanly.
        assert_eq!(
            engine.disconnect_device("xbox"),
            vec![GamepadOutput::Key {
                vk: 0x20,
                down: false
            }]
        );
    }

    #[test]
    fn sticks_tick_only_outside_deadzone_and_dpad_has_hysteresis() {
        let mut engine = GamepadEngine::new();
        let mut cfg = DeviceConfig::default();

        // Rested sticks require zero ticks.
        assert!(engine.on_stick("p1", false, 0.05, -0.08, &cfg).is_empty());
        assert!(!engine.needs_tick());

        // Deflected left stick (Cursor role) requests ticks and moves right.
        assert!(engine.on_stick("p1", false, 1.0, 0.0, &cfg).is_empty());
        assert!(engine.needs_tick());
        let moved = engine.tick(0.05);
        assert_eq!(moved.len(), 1);
        match moved[0] {
            GamepadOutput::MouseMove { dx, dy } => {
                assert!(dx > 20.0);
                assert!(dy.abs() < 0.01);
            }
            other => panic!("expected MouseMove, got {other:?}"),
        }

        // Returning to center immediately stops tick demand.
        assert!(engine.on_stick("p1", false, 0.0, 0.0, &cfg).is_empty());
        assert!(!engine.needs_tick());

        // Right stick wheel role emits an immediate notch on tilt up (-y).
        assert_eq!(
            engine.on_stick("p1", true, 0.0, -0.8, &cfg),
            vec![GamepadOutput::Wheel { notches: 1 }]
        );
        assert!(engine.needs_tick());
        assert!(engine.on_stick("p1", true, 0.0, 0.0, &cfg).is_empty());
        assert!(!engine.needs_tick());

        // Dpad role with WASD and hysteresis.
        cfg.profile.left_stick = StickRole::Dpad;
        cfg.profile.stick_dpad_bind = DpadBind::Wasd;
        assert_eq!(
            engine.on_stick("p1", false, 0.0, -0.6, &cfg),
            vec![GamepadOutput::Key {
                vk: 0x57,
                down: true
            }]
        );
        // Inside hysteresis band (0.35 > 0.30) keeps W held.
        assert!(engine.on_stick("p1", false, 0.0, -0.35, &cfg).is_empty());
        // Below 0.30 releases W.
        assert_eq!(
            engine.on_stick("p1", false, 0.0, -0.2, &cfg),
            vec![GamepadOutput::Key {
                vk: 0x57,
                down: false
            }]
        );
    }
}
