import { call, inTauri, onHostEvent } from "./bridge";
import { bindingCanLatch, type DpadBind, type KeyBinding } from "./key-layouts";
import {
  defaultBuiltinProfile,
  defaultGamepadConfig,
  dpadPresetBindings,
  normalizeBinding,
  normalizeGamepadConfig,
  type GamepadButtonId,
  type GamepadConfig,
  type GamepadPress,
  type GamepadProfile,
  type GamepadSnapshot,
  type GlyphOverride,
  type StickRole,
} from "./gamepad";

const STORAGE_KEY = "hitode.gamepad.preview";

function loadPreviewSnapshot(): GamepadSnapshot {
  const simulate =
    typeof location !== "undefined" && new URLSearchParams(location.search).has("gamepad");
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as GamepadSnapshot;
      if (parsed?.config && (!simulate || parsed.config.devices.length > 0)) {
        return {
          config: normalizeGamepadConfig(parsed.config),
          connectedIds: Array.isArray(parsed.connectedIds) ? parsed.connectedIds : [],
        };
      }
    }
  } catch {
    // Ignore invalid preview storage.
  }
  if (simulate) {
    return {
      config: normalizeGamepadConfig({
        ...defaultGamepadConfig(),
        unlocked: true,
        devices: [
          {
            id: "preview-xbox",
            name: "Xbox Wireless Controller",
            detectedStyle: "xbox",
            glyphOverride: "auto",
            swapConfirmCancel: false,
            defaultProfileId: null,
          },
          {
            id: "preview-switch",
            name: "Nintendo Switch Pro",
            detectedStyle: "switch",
            glyphOverride: "auto",
            swapConfirmCancel: true,
            defaultProfileId: null,
          },
        ],
      }),
      connectedIds: ["preview-xbox", "preview-switch"],
    };
  }
  return {
    config: defaultGamepadConfig(),
    connectedIds: [],
  };
}

class GamepadStore {
  config = $state<GamepadConfig>(defaultGamepadConfig());
  connectedIds = $state<string[]>([]);
  lastPress = $state<{ deviceId: string; button: GamepadButtonId; seq: number } | null>(null);
  #pressSeq = 0;

  /** Plan B: Settings entry unlocks after the first controller connection. */
  readonly showInSettings = $derived(this.config.unlocked || this.connectedIds.length > 0);
  /** Plan B: In-game pause menu entry appears only while a controller is connected. */
  readonly showInPauseMenu = $derived(this.connectedIds.length > 0);
  readonly hasConnected = $derived(this.connectedIds.length > 0);

  constructor() {
    if (inTauri) {
      void this.refresh();
      onHostEvent<GamepadSnapshot>("gamepad_state", (snap) => this.#apply(snap));
      onHostEvent<GamepadPress>("gamepad_press", (press) => {
        this.lastPress = { ...press, seq: ++this.#pressSeq };
      });
    } else if (typeof localStorage !== "undefined") {
      this.#apply(loadPreviewSnapshot());
    }
  }

  async refresh(): Promise<void> {
    if (!inTauri) return;
    const snap = await call<GamepadSnapshot | null>("gamepad_get_state", {}, null);
    if (snap) this.#apply(snap);
  }

  #apply(snap: GamepadSnapshot): void {
    this.config = normalizeGamepadConfig(snap.config);
    this.connectedIds = [...(snap.connectedIds ?? [])];
  }

  async #commit(next: GamepadConfig): Promise<void> {
    const normalized = normalizeGamepadConfig(next);
    this.config = normalized;
    if (inTauri) {
      const snap = await call<GamepadSnapshot | null>("gamepad_save_config", { config: normalized }, null);
      if (snap) this.#apply(snap);
    } else if (typeof localStorage !== "undefined") {
      try {
        localStorage.setItem(
          STORAGE_KEY,
          JSON.stringify({ config: normalized, connectedIds: this.connectedIds }),
        );
      } catch {
        // Ignore storage quota errors in preview mode.
      }
    }
  }

  #clone(): GamepadConfig {
    return JSON.parse(JSON.stringify(this.config)) as GamepadConfig;
  }

  isConnected(deviceId: string): boolean {
    return this.connectedIds.includes(deviceId);
  }

  profileById(id: string): GamepadProfile {
    return this.config.profiles.find((p) => p.id === id) ?? this.config.profiles[0]!;
  }

  setGlobalProfile(profileId: string): void {
    const next = this.#clone();
    next.globalProfileId = profileId;
    void this.#commit(next);
  }

  setGameProfile(gameId: string, profileId: string | null): void {
    const next = this.#clone();
    if (profileId === null) delete next.gameProfiles[gameId];
    else next.gameProfiles[gameId] = profileId;
    void this.#commit(next);
  }

  setDeviceProfile(deviceId: string, profileId: string | null): void {
    const next = this.#clone();
    const dev = next.devices.find((d) => d.id === deviceId);
    if (!dev) return;
    dev.defaultProfileId = profileId;
    void this.#commit(next);
  }

  setDeviceSwap(deviceId: string, swap: boolean): void {
    const next = this.#clone();
    const dev = next.devices.find((d) => d.id === deviceId);
    if (!dev) return;
    dev.swapConfirmCancel = swap;
    void this.#commit(next);
  }

  setDeviceGlyph(deviceId: string, glyph: GlyphOverride): void {
    const next = this.#clone();
    const dev = next.devices.find((d) => d.id === deviceId);
    if (!dev) return;
    dev.glyphOverride = glyph;
    void this.#commit(next);
  }

  forgetDevice(deviceId: string): void {
    if (this.isConnected(deviceId)) return;
    const next = this.#clone();
    next.devices = next.devices.filter((d) => d.id !== deviceId);
    void this.#commit(next);
  }

  setAutoHideTouch(enabled: boolean): void {
    const next = this.#clone();
    next.autoHideTouch = enabled;
    void this.#commit(next);
  }

  setStickRole(profileId: string, side: "leftStick" | "rightStick", role: StickRole): void {
    const next = this.#clone();
    const profile = next.profiles.find((p) => p.id === profileId);
    if (!profile) return;
    profile[side] = role;
    void this.#commit(next);
  }

  setCursorSpeed(profileId: string, speed: number): void {
    const next = this.#clone();
    const profile = next.profiles.find((p) => p.id === profileId);
    if (!profile) return;
    profile.cursorSpeed = speed;
    void this.#commit(next);
  }

  setStickDpadBind(profileId: string, bind: DpadBind): void {
    const next = this.#clone();
    const profile = next.profiles.find((p) => p.id === profileId);
    if (!profile) return;
    profile.stickDpadBind = bind;
    void this.#commit(next);
  }

  setButtonBinding(profileId: string, slot: GamepadButtonId, binding: KeyBinding | null): void {
    const next = this.#clone();
    const profile = next.profiles.find((p) => p.id === profileId);
    if (!profile) return;
    if (!binding) {
      delete profile.bindings[slot];
    } else {
      const normalized = normalizeBinding(binding);
      const prevLatch = profile.bindings[slot]?.latch ?? false;
      profile.bindings[slot] = {
        binding: normalized,
        latch: bindingCanLatch(normalized) ? prevLatch : false,
      };
    }
    void this.#commit(next);
  }

  setButtonLatch(profileId: string, slot: GamepadButtonId, latch: boolean): void {
    const next = this.#clone();
    const profile = next.profiles.find((p) => p.id === profileId);
    const entry = profile?.bindings[slot];
    if (!entry || !bindingCanLatch(entry.binding)) return;
    entry.latch = latch;
    void this.#commit(next);
  }

  applyDpadPreset(profileId: string, mode: DpadBind): void {
    const next = this.#clone();
    const profile = next.profiles.find((p) => p.id === profileId);
    if (!profile) return;
    Object.assign(profile.bindings, dpadPresetBindings(mode));
    void this.#commit(next);
  }

  resetBuiltinProfile(profileId: string): void {
    const base = defaultBuiltinProfile(profileId);
    if (!base) return;
    const next = this.#clone();
    const idx = next.profiles.findIndex((p) => p.id === profileId);
    if (idx >= 0) next.profiles[idx] = base;
    void this.#commit(next);
  }

  saveAsProfile(sourceId: string, name: string): string | null {
    const trimmed = name.trim();
    if (!trimmed) return null;
    const next = this.#clone();
    const src = next.profiles.find((p) => p.id === sourceId) ?? next.profiles[0];
    if (!src) return null;
    const id = `custom-${Date.now().toString(36)}`;
    next.profiles.push({
      ...JSON.parse(JSON.stringify(src)),
      id,
      name: trimmed,
      builtin: false,
    });
    void this.#commit(next);
    return id;
  }

  deleteProfile(profileId: string): void {
    const next = this.#clone();
    const target = next.profiles.find((p) => p.id === profileId);
    if (!target || target.builtin) return;
    next.profiles = next.profiles.filter((p) => p.id !== profileId);
    if (next.globalProfileId === profileId) next.globalProfileId = "adv";
    for (const [gid, pid] of Object.entries(next.gameProfiles)) {
      if (pid === profileId) delete next.gameProfiles[gid];
    }
    for (const dev of next.devices) {
      if (dev.defaultProfileId === profileId) dev.defaultProfileId = null;
    }
    void this.#commit(next);
  }
}

export const gamepad = new GamepadStore();
