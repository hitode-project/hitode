//! External gamepad configuration persistence, device registry, and runtime dispatch.
//!
//! Stores user preferences in `<app_data_dir>/gamepad.json` and drives
//! [`adv_shell::gamepad::GamepadEngine`] from native controller callbacks
//! (GameController on macOS/iOS, InputManager/MainActivity on Android).

use adv_shell::gamepad::{
    ButtonEntry, DeviceConfig, DpadBind, GamepadButton, GamepadEngine, GamepadOutput,
    GamepadProfile, GamepadStyle, KeyBinding, MouseButton, StickRole, SystemAction,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StoredKeyBinding {
    Vk {
        vk: u32,
        label: String,
    },
    Mouse {
        #[serde(default = "default_mouse_button")]
        button: String,
        label: String,
    },
    Wheel {
        notches: i32,
        label: String,
    },
    Sys {
        sys: String,
        label: String,
    },
}

fn default_mouse_button() -> String {
    "left".into()
}

fn default_true() -> bool {
    true
}

fn default_one() -> f32 {
    1.0
}

fn default_cursor() -> String {
    "cursor".into()
}

fn default_wheel() -> String {
    "wheel".into()
}

fn default_arrows() -> String {
    "arrows".into()
}

fn default_auto() -> String {
    "auto".into()
}

impl StoredKeyBinding {
    fn to_engine(&self) -> Option<KeyBinding> {
        match self {
            Self::Vk { vk, .. } => Some(KeyBinding::Vk(*vk)),
            Self::Mouse { button, .. } => {
                let btn = match button.as_str() {
                    "right" => MouseButton::Right,
                    "middle" => MouseButton::Middle,
                    _ => MouseButton::Left,
                };
                Some(KeyBinding::Mouse(btn))
            }
            Self::Wheel { notches, .. } => Some(KeyBinding::Wheel(*notches)),
            Self::Sys { sys, .. } => {
                let action = match sys.as_str() {
                    "menu" => SystemAction::Menu,
                    "hide" => SystemAction::Hide,
                    "kbd" => SystemAction::Kbd,
                    _ => return None,
                };
                Some(KeyBinding::Sys(action))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredButtonEntry {
    pub binding: StoredKeyBinding,
    #[serde(default)]
    pub latch: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default = "default_cursor")]
    pub left_stick: String,
    #[serde(default = "default_wheel")]
    pub right_stick: String,
    #[serde(default = "default_arrows")]
    pub stick_dpad_bind: String,
    #[serde(default = "default_one")]
    pub cursor_speed: f32,
    #[serde(default)]
    pub bindings: BTreeMap<String, StoredButtonEntry>,
}

impl StoredProfile {
    fn to_engine(&self) -> GamepadProfile {
        let parse_role = |s: &str| match s {
            "cursor" => StickRole::Cursor,
            "dpad" => StickRole::Dpad,
            "wheel" => StickRole::Wheel,
            _ => StickRole::Off,
        };
        let dpad_bind = match self.stick_dpad_bind.as_str() {
            "wasd" => DpadBind::Wasd,
            "numpad" => DpadBind::Numpad,
            _ => DpadBind::Arrows,
        };
        let mut bindings = BTreeMap::new();
        for (key, entry) in &self.bindings {
            if let (Some(btn), Some(binding)) =
                (GamepadButton::parse(key), entry.binding.to_engine())
            {
                bindings.insert(
                    btn,
                    ButtonEntry {
                        binding,
                        latch: entry.latch && binding.can_latch(),
                    },
                );
            }
        }
        GamepadProfile {
            left_stick: parse_role(&self.left_stick),
            right_stick: parse_role(&self.right_stick),
            stick_dpad_bind: dpad_bind,
            cursor_speed: if self.cursor_speed.is_finite() {
                self.cursor_speed.clamp(0.5, 2.0)
            } else {
                1.0
            },
            bindings,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredDevice {
    pub id: String,
    pub name: String,
    pub detected_style: String,
    #[serde(default = "default_auto")]
    pub glyph_override: String,
    #[serde(default)]
    pub swap_confirm_cancel: bool,
    #[serde(default)]
    pub default_profile_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GamepadConfig {
    #[serde(default)]
    pub unlocked: bool,
    #[serde(default = "default_profile_id")]
    pub global_profile_id: String,
    #[serde(default)]
    pub game_profiles: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub auto_hide_touch: bool,
    #[serde(default)]
    pub devices: Vec<StoredDevice>,
    #[serde(default)]
    pub profiles: Vec<StoredProfile>,
}

fn default_profile_id() -> String {
    "adv".into()
}

impl Default for GamepadConfig {
    fn default() -> Self {
        Self {
            unlocked: false,
            global_profile_id: "adv".into(),
            game_profiles: BTreeMap::new(),
            auto_hide_touch: true,
            devices: Vec::new(),
            profiles: builtin_profiles(),
        }
    }
}

fn vk(code: u32, label: &str) -> StoredButtonEntry {
    StoredButtonEntry {
        binding: StoredKeyBinding::Vk {
            vk: code,
            label: label.into(),
        },
        latch: false,
    }
}

fn vk_latch(code: u32, label: &str) -> StoredButtonEntry {
    StoredButtonEntry {
        binding: StoredKeyBinding::Vk {
            vk: code,
            label: label.into(),
        },
        latch: true,
    }
}

fn mouse(button: &str, label: &str) -> StoredButtonEntry {
    StoredButtonEntry {
        binding: StoredKeyBinding::Mouse {
            button: button.into(),
            label: label.into(),
        },
        latch: false,
    }
}

fn wheel(notches: i32, label: &str) -> StoredButtonEntry {
    StoredButtonEntry {
        binding: StoredKeyBinding::Wheel {
            notches,
            label: label.into(),
        },
        latch: false,
    }
}

fn sys(name: &str, label: &str) -> StoredButtonEntry {
    StoredButtonEntry {
        binding: StoredKeyBinding::Sys {
            sys: name.into(),
            label: label.into(),
        },
        latch: false,
    }
}

pub fn builtin_profiles() -> Vec<StoredProfile> {
    let mut adv = BTreeMap::new();
    adv.insert("south".into(), mouse("left", "左键"));
    adv.insert("east".into(), mouse("right", "右键"));
    adv.insert("west".into(), vk(0x20, "Space"));
    adv.insert("north".into(), vk(0x0d, "Enter"));
    adv.insert("l1".into(), vk(0x1b, "Esc"));
    adv.insert("r1".into(), vk_latch(0x11, "Ctrl"));
    adv.insert("l2".into(), wheel(1, "滚轮 ↑"));
    adv.insert("r2".into(), wheel(-1, "滚轮 ↓"));
    adv.insert("r3".into(), mouse("middle", "中键"));
    adv.insert("dpadUp".into(), vk(0x26, "↑"));
    adv.insert("dpadDown".into(), vk(0x28, "↓"));
    adv.insert("dpadLeft".into(), vk(0x25, "←"));
    adv.insert("dpadRight".into(), vk(0x27, "→"));
    adv.insert("start".into(), sys("menu", "菜单"));
    adv.insert("select".into(), sys("kbd", "键盘"));

    let mut action = BTreeMap::new();
    action.insert("south".into(), vk(0x5a, "Z"));
    action.insert("east".into(), vk(0x58, "X"));
    action.insert("west".into(), vk(0x43, "C"));
    action.insert("north".into(), vk(0x10, "Shift"));
    action.insert("l1".into(), vk(0x1b, "Esc"));
    action.insert("r1".into(), vk(0x11, "Ctrl"));
    action.insert("l2".into(), vk(0x09, "Tab"));
    action.insert("r2".into(), vk(0x20, "Space"));
    action.insert("dpadUp".into(), vk(0x26, "↑"));
    action.insert("dpadDown".into(), vk(0x28, "↓"));
    action.insert("dpadLeft".into(), vk(0x25, "←"));
    action.insert("dpadRight".into(), vk(0x27, "→"));
    action.insert("start".into(), sys("menu", "菜单"));
    action.insert("select".into(), vk(0x0d, "Enter"));

    let mut mouse_only = BTreeMap::new();
    mouse_only.insert("south".into(), mouse("left", "左键"));
    mouse_only.insert("east".into(), mouse("right", "右键"));
    mouse_only.insert("west".into(), mouse("middle", "中键"));
    mouse_only.insert("north".into(), vk_latch(0x11, "Ctrl"));
    mouse_only.insert("l1".into(), mouse("left", "左键"));
    mouse_only.insert("r1".into(), mouse("right", "右键"));
    mouse_only.insert("l2".into(), wheel(1, "滚轮 ↑"));
    mouse_only.insert("r2".into(), wheel(-1, "滚轮 ↓"));
    mouse_only.insert("l3".into(), mouse("left", "左键"));
    mouse_only.insert("r3".into(), mouse("right", "右键"));
    mouse_only.insert("dpadUp".into(), wheel(1, "滚轮 ↑"));
    mouse_only.insert("dpadDown".into(), wheel(-1, "滚轮 ↓"));
    mouse_only.insert("start".into(), sys("menu", "菜单"));
    mouse_only.insert("select".into(), sys("kbd", "键盘"));

    vec![
        StoredProfile {
            id: "adv".into(),
            name: "视觉小说".into(),
            builtin: true,
            left_stick: "cursor".into(),
            right_stick: "wheel".into(),
            stick_dpad_bind: "arrows".into(),
            cursor_speed: 1.0,
            bindings: adv,
        },
        StoredProfile {
            id: "action".into(),
            name: "键盘动作".into(),
            builtin: true,
            left_stick: "dpad".into(),
            right_stick: "cursor".into(),
            stick_dpad_bind: "arrows".into(),
            cursor_speed: 1.0,
            bindings: action,
        },
        StoredProfile {
            id: "mouseOnly".into(),
            name: "单手 / 纯鼠标".into(),
            builtin: true,
            left_stick: "cursor".into(),
            right_stick: "wheel".into(),
            stick_dpad_bind: "arrows".into(),
            cursor_speed: 1.2,
            bindings: mouse_only,
        },
    ]
}

impl GamepadConfig {
    fn normalize(&mut self) {
        let builtins = builtin_profiles();
        for b in builtins {
            if !self.profiles.iter().any(|p| p.id == b.id) {
                self.profiles.push(b);
            }
        }
        if !self.profiles.iter().any(|p| p.id == self.global_profile_id) {
            self.global_profile_id = "adv".into();
        }
        for p in &mut self.profiles {
            if !p.cursor_speed.is_finite() {
                p.cursor_speed = 1.0;
            } else {
                p.cursor_speed = p.cursor_speed.clamp(0.5, 2.0);
            }
        }
        let valid_ids: std::collections::HashSet<&str> =
            self.profiles.iter().map(|p| p.id.as_str()).collect();
        self.game_profiles
            .retain(|_, pid| valid_ids.contains(pid.as_str()));
        for dev in &mut self.devices {
            if let Some(pid) = &dev.default_profile_id
                && !valid_ids.contains(pid.as_str())
            {
                dev.default_profile_id = None;
            }
        }
    }

    fn resolve_device_config(&self, device_id: &str, game_id: Option<&str>) -> DeviceConfig {
        let device = self.devices.iter().find(|d| d.id == device_id);
        let profile_id = game_id
            .and_then(|gid| self.game_profiles.get(gid))
            .map(String::as_str)
            .or_else(|| device.and_then(|d| d.default_profile_id.as_deref()))
            .unwrap_or(&self.global_profile_id);
        let profile = self
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .or_else(|| self.profiles.first())
            .map(StoredProfile::to_engine)
            .unwrap_or_default();
        DeviceConfig {
            swap_confirm_cancel: device.is_some_and(|d| d.swap_confirm_cancel),
            profile,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamepadSnapshot {
    pub config: GamepadConfig,
    pub connected_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GamepadPressEvent {
    device_id: String,
    button: &'static str,
}

struct Runtime {
    path: PathBuf,
    config: GamepadConfig,
    connected: Vec<String>,
    engine: GamepadEngine,
    last_tick: Option<Instant>,
}

impl Runtime {
    fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("gamepad.json");
        let mut config = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<GamepadConfig>(&s).ok())
            .unwrap_or_default();
        config.normalize();
        Self {
            path,
            config,
            connected: Vec::new(),
            engine: GamepadEngine::new(),
            last_tick: None,
        }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(&self.config).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    fn snapshot(&self) -> GamepadSnapshot {
        GamepadSnapshot {
            config: self.config.clone(),
            connected_ids: self.connected.clone(),
        }
    }
}

static APP: OnceLock<AppHandle> = OnceLock::new();
static RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();

pub fn init(app: AppHandle, data_dir: &Path) {
    let _ = APP.set(app);
    let _ = RUNTIME.set(Mutex::new(Runtime::load(data_dir)));
}

fn emit_state(snapshot: &GamepadSnapshot) {
    if let Some(app) = APP.get() {
        let _ = app.emit("gamepad_state", snapshot);
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
fn dispatch_outputs(outputs: Vec<GamepadOutput>) {
    for output in outputs {
        match output {
            GamepadOutput::System(action) => {
                crate::game::wake_overlay_for_gamepad();
                if let Some(app) = APP.get() {
                    let _ = app.emit("gamepad_sys", action.as_str());
                }
            }
            other => crate::game::dispatch_gamepad_output(other),
        }
    }
}

/// Registers a connected controller (Plan B: unlocks settings on first connection).
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
pub fn on_device_connected(id: String, name: String, style: GamepadStyle) {
    let Some(mutex) = RUNTIME.get() else {
        return;
    };
    let snapshot = {
        let mut rt = mutex.lock().unwrap();
        let mut dirty = false;
        if !rt.config.unlocked {
            rt.config.unlocked = true;
            dirty = true;
        }
        if let Some(existing) = rt.config.devices.iter_mut().find(|d| d.id == id) {
            if existing.name != name {
                existing.name = name;
                dirty = true;
            }
            if existing.detected_style != style.as_str() {
                existing.detected_style = style.as_str().into();
                dirty = true;
            }
        } else {
            rt.config.devices.push(StoredDevice {
                id: id.clone(),
                name,
                detected_style: style.as_str().into(),
                glyph_override: "auto".into(),
                swap_confirm_cancel: style == GamepadStyle::Switch,
                default_profile_id: None,
            });
            dirty = true;
        }
        if !rt.connected.contains(&id) {
            rt.connected.push(id);
        }
        if dirty {
            let _ = rt.save();
        }
        rt.snapshot()
    };
    emit_state(&snapshot);
}

/// Unregisters a disconnected controller and releases any held outputs it owned.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
pub fn on_device_disconnected(id: &str) {
    let Some(mutex) = RUNTIME.get() else {
        return;
    };
    let (outputs, snapshot) = {
        let mut rt = mutex.lock().unwrap();
        rt.connected.retain(|x| x != id);
        let outputs = rt.engine.disconnect_device(id);
        (outputs, rt.snapshot())
    };
    dispatch_outputs(outputs);
    emit_state(&snapshot);
}

/// Handles a physical button transition on the host main/UI thread.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
pub fn on_button(device_id: &str, button: GamepadButton, down: bool) {
    let Some(mutex) = RUNTIME.get() else {
        return;
    };
    let (game_id, paused) = crate::game::gamepad_game_state();
    let in_ui = game_id.is_none() || paused;

    if down
        && in_ui
        && let Some(app) = APP.get()
    {
        let _ = app.emit(
            "gamepad_press",
            GamepadPressEvent {
                device_id: device_id.to_string(),
                button: button.as_str(),
            },
        );
    }

    let Some(gid) = game_id else {
        return;
    };

    let outputs = {
        let mut rt = mutex.lock().unwrap();
        let cfg = rt.config.resolve_device_config(device_id, Some(&gid));
        if paused {
            // While paused, only a bound Menu button can toggle back to gameplay.
            if down {
                let slot = if cfg.swap_confirm_cancel {
                    match button {
                        GamepadButton::South => GamepadButton::East,
                        GamepadButton::East => GamepadButton::South,
                        other => other,
                    }
                } else {
                    button
                };
                if cfg
                    .profile
                    .bindings
                    .get(&slot)
                    .is_some_and(|e| e.binding == KeyBinding::Sys(SystemAction::Menu))
                {
                    vec![GamepadOutput::System(SystemAction::Menu)]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        } else {
            rt.engine.on_button(device_id, button, down, &cfg)
        }
    };
    dispatch_outputs(outputs);
}

/// Handles a stick axis update (`x`: +right, `y`: +down). Returns whether the
/// host should keep its 60 Hz stick tick timer running.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
pub fn on_stick(device_id: &str, right_stick: bool, x: f32, y: f32) -> bool {
    let Some(mutex) = RUNTIME.get() else {
        return false;
    };
    let (game_id, paused) = crate::game::gamepad_game_state();
    let Some(gid) = game_id else {
        return false;
    };
    if paused {
        return false;
    }
    let (outputs, needs_tick) = {
        let mut rt = mutex.lock().unwrap();
        let cfg = rt.config.resolve_device_config(device_id, Some(&gid));
        let outputs = rt.engine.on_stick(device_id, right_stick, x, y, &cfg);
        let needs_tick = rt.engine.needs_tick();
        if needs_tick && rt.last_tick.is_none() {
            rt.last_tick = Some(Instant::now());
        } else if !needs_tick {
            rt.last_tick = None;
        }
        (outputs, needs_tick)
    };
    dispatch_outputs(outputs);
    needs_tick
}

/// Advances continuous cursor/wheel stick outputs. Returns whether ticks must continue.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
pub fn on_tick() -> bool {
    let Some(mutex) = RUNTIME.get() else {
        return false;
    };
    let (game_id, paused) = crate::game::gamepad_game_state();
    if game_id.is_none() || paused {
        on_pause_or_stop();
        return false;
    }
    let (outputs, needs_tick) = {
        let mut rt = mutex.lock().unwrap();
        let now = Instant::now();
        let dt = rt
            .last_tick
            .replace(now)
            .map_or(1.0 / 60.0, |prev| now.duration_since(prev).as_secs_f32());
        let outputs = rt.engine.tick(dt);
        let needs_tick = rt.engine.needs_tick();
        if !needs_tick {
            rt.last_tick = None;
        }
        (outputs, needs_tick)
    };
    dispatch_outputs(outputs);
    needs_tick
}

/// Releases all held and latched keys/buttons when pausing or leaving a game.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
pub fn on_pause_or_stop() {
    let Some(mutex) = RUNTIME.get() else {
        return;
    };
    let outputs = {
        let mut rt = mutex.lock().unwrap();
        rt.last_tick = None;
        rt.engine.reset_all()
    };
    dispatch_outputs(outputs);
}

#[tauri::command]
pub fn gamepad_get_state() -> Result<GamepadSnapshot, String> {
    let rt = RUNTIME
        .get()
        .ok_or_else(|| "gamepad runtime is not initialized".to_string())?
        .lock()
        .unwrap();
    Ok(rt.snapshot())
}

#[tauri::command]
pub fn gamepad_save_config(mut config: GamepadConfig) -> Result<GamepadSnapshot, String> {
    config.normalize();
    let (outputs, snapshot) = {
        let mut rt = RUNTIME
            .get()
            .ok_or_else(|| "gamepad runtime is not initialized".to_string())?
            .lock()
            .unwrap();
        rt.config = config;
        rt.save()?;
        rt.last_tick = None;
        let outputs = rt.engine.reset_all();
        (outputs, rt.snapshot())
    };
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
    dispatch_outputs(outputs);
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
    let _ = outputs;
    emit_state(&snapshot);
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_preserves_devices_and_profile_precedence() {
        let mut cfg = GamepadConfig {
            unlocked: true,
            ..Default::default()
        };
        cfg.devices.push(StoredDevice {
            id: "dev-switch".into(),
            name: "Switch Pro".into(),
            detected_style: "switch".into(),
            glyph_override: "auto".into(),
            swap_confirm_cancel: true,
            default_profile_id: Some("mouseOnly".into()),
        });
        cfg.game_profiles.insert("g1".into(), "action".into());
        cfg.normalize();

        // Game override takes precedence over device default.
        let g1_cfg = cfg.resolve_device_config("dev-switch", Some("g1"));
        assert!(g1_cfg.swap_confirm_cancel);
        assert_eq!(g1_cfg.profile.left_stick, StickRole::Dpad);

        // Without game override, device default wins over global default.
        let g2_cfg = cfg.resolve_device_config("dev-switch", Some("g2"));
        assert_eq!(g2_cfg.profile.cursor_speed, 1.2);

        // JSON serialization round-trips cleanly.
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: GamepadConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cfg);
    }
}
