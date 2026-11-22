import type { DpadBind, KeyBinding, KeyCategory } from "./key-layouts";

export type GamepadButtonId =
  | "south"
  | "east"
  | "west"
  | "north"
  | "l1"
  | "r1"
  | "l2"
  | "r2"
  | "l3"
  | "r3"
  | "dpadUp"
  | "dpadDown"
  | "dpadLeft"
  | "dpadRight"
  | "start"
  | "select";

export type GamepadStyle = "xbox" | "ps" | "switch";
export type GlyphOverride = "auto" | GamepadStyle;
export type StickRole = "cursor" | "dpad" | "wheel" | "off";

export interface GamepadButtonEntry {
  binding: KeyBinding;
  latch: boolean;
}

export interface GamepadProfile {
  id: string;
  name: string;
  builtin: boolean;
  leftStick: StickRole;
  rightStick: StickRole;
  stickDpadBind: DpadBind;
  cursorSpeed: number;
  bindings: Partial<Record<GamepadButtonId, GamepadButtonEntry>>;
}

export interface GamepadDevice {
  id: string;
  name: string;
  detectedStyle: GamepadStyle;
  glyphOverride: GlyphOverride;
  swapConfirmCancel: boolean;
  defaultProfileId: string | null;
}

export interface GamepadConfig {
  unlocked: boolean;
  globalProfileId: string;
  gameProfiles: Record<string, string>;
  autoHideTouch: boolean;
  devices: GamepadDevice[];
  profiles: GamepadProfile[];
}

export interface GamepadSnapshot {
  config: GamepadConfig;
  connectedIds: string[];
}

export interface GamepadPress {
  deviceId: string;
  button: GamepadButtonId;
}

export interface ButtonGlyph {
  glyph: string;
  name: string;
  shape: "round" | "pill";
}

export const BUTTON_GROUPS: {
  id: string;
  title: string;
  buttons: GamepadButtonId[];
  hasDpadPresets?: boolean;
}[] = [
  { id: "face", title: "动作键", buttons: ["south", "east", "west", "north"] },
  { id: "shoulder", title: "肩键与扳机", buttons: ["l1", "r1", "l2", "r2", "l3", "r3"] },
  {
    id: "dpad",
    title: "十字键",
    buttons: ["dpadUp", "dpadDown", "dpadLeft", "dpadRight"],
    hasDpadPresets: true,
  },
  { id: "meta", title: "功能键", buttons: ["start", "select"] },
];

export const CATEGORY_TABS: readonly [KeyCategory, string][] = [
  ["common", "常用"],
  ["alpha", "字母"],
  ["symbol", "符号"],
  ["fn", "功能"],
  ["numpad", "小键"],
  ["system", "系统"],
];

export const GLYPH_STYLES: Record<GamepadStyle, Record<GamepadButtonId, ButtonGlyph>> = {
  xbox: {
    south: { glyph: "A", name: "A 键", shape: "round" },
    east: { glyph: "B", name: "B 键", shape: "round" },
    west: { glyph: "X", name: "X 键", shape: "round" },
    north: { glyph: "Y", name: "Y 键", shape: "round" },
    l1: { glyph: "LB", name: "左肩键 LB", shape: "pill" },
    r1: { glyph: "RB", name: "右肩键 RB", shape: "pill" },
    l2: { glyph: "LT", name: "左扳机 LT", shape: "pill" },
    r2: { glyph: "RT", name: "右扳机 RT", shape: "pill" },
    l3: { glyph: "LS", name: "左摇杆按下", shape: "round" },
    r3: { glyph: "RS", name: "右摇杆按下", shape: "round" },
    dpadUp: { glyph: "↑", name: "十字键 上", shape: "pill" },
    dpadDown: { glyph: "↓", name: "十字键 下", shape: "pill" },
    dpadLeft: { glyph: "←", name: "十字键 左", shape: "pill" },
    dpadRight: { glyph: "→", name: "十字键 右", shape: "pill" },
    start: { glyph: "≡", name: "Menu 键", shape: "pill" },
    select: { glyph: "⧉", name: "View 键", shape: "pill" },
  },
  ps: {
    south: { glyph: "×", name: "× 键", shape: "round" },
    east: { glyph: "○", name: "○ 键", shape: "round" },
    west: { glyph: "□", name: "□ 键", shape: "round" },
    north: { glyph: "△", name: "△ 键", shape: "round" },
    l1: { glyph: "L1", name: "L1 键", shape: "pill" },
    r1: { glyph: "R1", name: "R1 键", shape: "pill" },
    l2: { glyph: "L2", name: "L2 扳机", shape: "pill" },
    r2: { glyph: "R2", name: "R2 扳机", shape: "pill" },
    l3: { glyph: "L3", name: "左摇杆按下", shape: "round" },
    r3: { glyph: "R3", name: "右摇杆按下", shape: "round" },
    dpadUp: { glyph: "↑", name: "方向键 上", shape: "pill" },
    dpadDown: { glyph: "↓", name: "方向键 下", shape: "pill" },
    dpadLeft: { glyph: "←", name: "方向键 左", shape: "pill" },
    dpadRight: { glyph: "→", name: "方向键 右", shape: "pill" },
    start: { glyph: "Options", name: "Options 键", shape: "pill" },
    select: { glyph: "Create", name: "Create 键", shape: "pill" },
  },
  switch: {
    south: { glyph: "B", name: "下方 B 键", shape: "round" },
    east: { glyph: "A", name: "右侧 A 键", shape: "round" },
    west: { glyph: "Y", name: "左侧 Y 键", shape: "round" },
    north: { glyph: "X", name: "上方 X 键", shape: "round" },
    l1: { glyph: "L", name: "L 键", shape: "pill" },
    r1: { glyph: "R", name: "R 键", shape: "pill" },
    l2: { glyph: "ZL", name: "ZL 扳机", shape: "pill" },
    r2: { glyph: "ZR", name: "ZR 扳机", shape: "pill" },
    l3: { glyph: "L3", name: "左摇杆按下", shape: "round" },
    r3: { glyph: "R3", name: "右摇杆按下", shape: "round" },
    dpadUp: { glyph: "↑", name: "十字键 上", shape: "pill" },
    dpadDown: { glyph: "↓", name: "十字键 下", shape: "pill" },
    dpadLeft: { glyph: "←", name: "十字键 左", shape: "pill" },
    dpadRight: { glyph: "→", name: "十字键 右", shape: "pill" },
    start: { glyph: "+", name: "+ 键", shape: "pill" },
    select: { glyph: "−", name: "− 键", shape: "pill" },
  },
};

function vk(code: number, label: string, latch = false): GamepadButtonEntry {
  return { binding: { kind: "vk", vk: code, label }, latch };
}

function mouse(button: "left" | "middle" | "right", label: string): GamepadButtonEntry {
  return { binding: { kind: "mouse", button, label }, latch: false };
}

function wheel(notches: number, label: string): GamepadButtonEntry {
  return { binding: { kind: "wheel", notches, label }, latch: false };
}

function sys(name: "menu" | "hide" | "kbd", label: string): GamepadButtonEntry {
  return { binding: { kind: "sys", sys: name, label }, latch: false };
}

export function builtinGamepadProfiles(): GamepadProfile[] {
  return [
    {
      id: "adv",
      name: "视觉小说",
      builtin: true,
      leftStick: "cursor",
      rightStick: "wheel",
      stickDpadBind: "arrows",
      cursorSpeed: 1.0,
      bindings: {
        south: mouse("left", "左键"),
        east: mouse("right", "右键"),
        west: vk(0x20, "Space"),
        north: vk(0x0d, "Enter"),
        l1: vk(0x1b, "Esc"),
        r1: vk(0x11, "Ctrl", true),
        l2: wheel(1, "滚轮 ↑"),
        r2: wheel(-1, "滚轮 ↓"),
        r3: mouse("middle", "中键"),
        dpadUp: vk(0x26, "↑"),
        dpadDown: vk(0x28, "↓"),
        dpadLeft: vk(0x25, "←"),
        dpadRight: vk(0x27, "→"),
        start: sys("menu", "菜单"),
        select: sys("kbd", "键盘"),
      },
    },
    {
      id: "action",
      name: "键盘动作",
      builtin: true,
      leftStick: "dpad",
      rightStick: "cursor",
      stickDpadBind: "arrows",
      cursorSpeed: 1.0,
      bindings: {
        south: vk(0x5a, "Z"),
        east: vk(0x58, "X"),
        west: vk(0x43, "C"),
        north: vk(0x10, "Shift"),
        l1: vk(0x1b, "Esc"),
        r1: vk(0x11, "Ctrl"),
        l2: vk(0x09, "Tab"),
        r2: vk(0x20, "Space"),
        dpadUp: vk(0x26, "↑"),
        dpadDown: vk(0x28, "↓"),
        dpadLeft: vk(0x25, "←"),
        dpadRight: vk(0x27, "→"),
        start: sys("menu", "菜单"),
        select: vk(0x0d, "Enter"),
      },
    },
    {
      id: "mouseOnly",
      name: "单手 / 纯鼠标",
      builtin: true,
      leftStick: "cursor",
      rightStick: "wheel",
      stickDpadBind: "arrows",
      cursorSpeed: 1.2,
      bindings: {
        south: mouse("left", "左键"),
        east: mouse("right", "右键"),
        west: mouse("middle", "中键"),
        north: vk(0x11, "Ctrl", true),
        l1: mouse("left", "左键"),
        r1: mouse("right", "右键"),
        l2: wheel(1, "滚轮 ↑"),
        r2: wheel(-1, "滚轮 ↓"),
        l3: mouse("left", "左键"),
        r3: mouse("right", "右键"),
        dpadUp: wheel(1, "滚轮 ↑"),
        dpadDown: wheel(-1, "滚轮 ↓"),
        start: sys("menu", "菜单"),
        select: sys("kbd", "键盘"),
      },
    },
  ];
}

export function defaultGamepadConfig(): GamepadConfig {
  return {
    unlocked: false,
    globalProfileId: "adv",
    gameProfiles: {},
    autoHideTouch: true,
    devices: [],
    profiles: builtinGamepadProfiles(),
  };
}

export function defaultBuiltinProfile(id: string): GamepadProfile | null {
  return builtinGamepadProfiles().find((p) => p.id === id) ?? null;
}

function mouseBtn(b: Extract<KeyBinding, { kind: "mouse" }>): "left" | "middle" | "right" {
  if (b.button === "left" || b.button === "middle" || b.button === "right") return b.button;
  return b.right ? "right" : "left";
}

function canLatch(binding: KeyBinding): boolean {
  return binding.kind === "vk" || binding.kind === "mouse";
}

function sameBinding(a: KeyBinding, b: KeyBinding): boolean {
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case "vk":
      return b.kind === "vk" && a.vk === b.vk;
    case "mouse":
      return b.kind === "mouse" && mouseBtn(a) === mouseBtn(b);
    case "wheel":
      return b.kind === "wheel" && a.notches === b.notches;
    case "sys":
      return b.kind === "sys" && a.sys === b.sys;
  }
}

export function isBuiltinModified(profile: GamepadProfile): boolean {
  if (!profile.builtin) return false;
  const base = defaultBuiltinProfile(profile.id);
  if (!base) return false;
  if (
    profile.leftStick !== base.leftStick ||
    profile.rightStick !== base.rightStick ||
    profile.stickDpadBind !== base.stickDpadBind ||
    Math.abs(profile.cursorSpeed - base.cursorSpeed) > 0.001
  ) {
    return true;
  }
  const allButtons = BUTTON_GROUPS.flatMap((g) => g.buttons);
  for (const btn of allButtons) {
    const a = profile.bindings[btn];
    const b = base.bindings[btn];
    if (!a && !b) continue;
    if (!a || !b) return true;
    if (a.latch !== b.latch || !sameBinding(a.binding, b.binding)) return true;
  }
  return false;
}

export function effectiveGlyphStyle(device: GamepadDevice | null): GamepadStyle {
  if (!device) return "xbox";
  return device.glyphOverride === "auto" ? device.detectedStyle : device.glyphOverride;
}

export function glyphsForDevice(device: GamepadDevice | null): Record<GamepadButtonId, ButtonGlyph> {
  return GLYPH_STYLES[effectiveGlyphStyle(device)];
}

export function effectiveButtonSlot(button: GamepadButtonId, swapConfirmCancel: boolean): GamepadButtonId {
  if (!swapConfirmCancel) return button;
  if (button === "south") return "east";
  if (button === "east") return "south";
  return button;
}

export function resolveEffectiveProfileId(
  config: GamepadConfig,
  deviceId: string | null,
  gameId: string | null,
): string {
  if (gameId && config.gameProfiles[gameId]) {
    return config.gameProfiles[gameId];
  }
  if (deviceId) {
    const dev = config.devices.find((d) => d.id === deviceId);
    if (dev?.defaultProfileId) return dev.defaultProfileId;
  }
  return config.globalProfileId;
}

export function findKeyCategory(
  binding: KeyBinding | null | undefined,
  catalog: Record<KeyCategory, KeyBinding[]>,
): KeyCategory {
  if (!binding) return "common";
  for (const [cat] of CATEGORY_TABS) {
    if (catalog[cat].some((k) => sameBinding(k, binding))) return cat;
  }
  return "common";
}

export function normalizeBinding(binding: KeyBinding): KeyBinding {
  if (binding.kind === "mouse") {
    return { kind: "mouse", button: mouseBtn(binding), label: binding.label };
  }
  return binding;
}

export function dpadPresetBindings(mode: DpadBind): Record<"dpadUp" | "dpadDown" | "dpadLeft" | "dpadRight", GamepadButtonEntry> {
  if (mode === "wasd") {
    return {
      dpadUp: vk(0x57, "W"),
      dpadDown: vk(0x53, "S"),
      dpadLeft: vk(0x41, "A"),
      dpadRight: vk(0x44, "D"),
    };
  }
  if (mode === "numpad") {
    return {
      dpadUp: vk(0x68, "Num 8"),
      dpadDown: vk(0x62, "Num 2"),
      dpadLeft: vk(0x64, "Num 4"),
      dpadRight: vk(0x66, "Num 6"),
    };
  }
  return {
    dpadUp: vk(0x26, "↑"),
    dpadDown: vk(0x28, "↓"),
    dpadLeft: vk(0x25, "←"),
    dpadRight: vk(0x27, "→"),
  };
}

export function normalizeGamepadConfig(raw: GamepadConfig): GamepadConfig {
  const profiles: GamepadProfile[] = raw.profiles.map((p) => {
    const bindings: Partial<Record<GamepadButtonId, GamepadButtonEntry>> = {};
    for (const [k, entry] of Object.entries(p.bindings ?? {})) {
      if (!entry?.binding) continue;
      const b = normalizeBinding(entry.binding);
      bindings[k as GamepadButtonId] = {
        binding: b,
        latch: Boolean(entry.latch) && canLatch(b),
      };
    }
    return {
      ...p,
      cursorSpeed: Number.isFinite(p.cursorSpeed)
        ? Math.min(2.0, Math.max(0.5, Math.round(p.cursorSpeed * 10) / 10))
        : 1.0,
      bindings,
    };
  });
  for (const b of builtinGamepadProfiles()) {
    if (!profiles.some((p) => p.id === b.id)) profiles.push(b);
  }
  const validIds = new Set(profiles.map((p) => p.id));
  const globalProfileId = validIds.has(raw.globalProfileId) ? raw.globalProfileId : "adv";
  const gameProfiles: Record<string, string> = {};
  for (const [gid, pid] of Object.entries(raw.gameProfiles ?? {})) {
    if (validIds.has(pid)) gameProfiles[gid] = pid;
  }
  const devices: GamepadDevice[] = (raw.devices ?? []).map((d) => ({
    ...d,
    defaultProfileId: d.defaultProfileId && validIds.has(d.defaultProfileId) ? d.defaultProfileId : null,
  }));
  return {
    unlocked: Boolean(raw.unlocked),
    globalProfileId,
    gameProfiles,
    autoHideTouch: raw.autoHideTouch !== false,
    devices,
    profiles,
  };
}
