import type { Area, GameLayout } from "./screen.svelte";

function cornerInset(radius: number, depth: number): number {
  if (depth >= radius) return 0;
  const d = radius - depth;
  return radius - Math.sqrt(radius * radius - d * d);
}

export type SystemKeyKind = "menu" | "hide" | "kbd";
export type MouseKeyButton = "left" | "middle" | "right";

export type KeyBinding =
  | { kind: "vk"; vk: number; label: string }
  | { kind: "mouse"; button?: MouseKeyButton; right?: boolean; label: string }
  | { kind: "wheel"; notches: number; label: string }
  | { kind: "sys"; sys: SystemKeyKind; label: string };

export function mouseButtonOf(binding: Extract<KeyBinding, { kind: "mouse" }>): MouseKeyButton {
  if (binding.button === "left" || binding.button === "middle" || binding.button === "right") {
    return binding.button;
  }
  return binding.right ? "right" : "left";
}

export interface KeyItem {
  id: string;
  type: "key";
  rx: number;
  ry: number;
  scale: number;
  latch: boolean;
  binding: KeyBinding;
}

export type DpadBind = "arrows" | "wasd" | "numpad";

export interface DpadItem {
  id: string;
  type: "dpad";
  rx: number;
  ry: number;
  scale: number;
  bind: DpadBind;
  eightWay: boolean;
}

export type OverlayItem = KeyItem | DpadItem;

export interface KeyProfile {
  id: string;
  name: string;
  builtin?: boolean;
  opacity: number;
  autoDim: boolean;
  items: OverlayItem[];
}

export interface StoredKeyLayouts {
  profiles: KeyProfile[];
  games: Record<string, string>;
}

export const MIN_OPACITY = 0.15;
export const MAX_OPACITY = 1.0;
export const MIN_SCALE = 0.65;
export const MAX_SCALE = 1.6;
export const BASE_KEY_SIZE = 46;
export const BASE_DPAD_SIZE = 124;
const PAD = 8;

export type KeyCategory = "common" | "alpha" | "symbol" | "fn" | "numpad" | "system";

export const KEY_CATALOG: Record<KeyCategory, KeyBinding[]> = {
  common: [
    { kind: "vk", vk: 0x0d, label: "Enter" },
    { kind: "vk", vk: 0x1b, label: "Esc" },
    { kind: "vk", vk: 0x20, label: "Space" },
    { kind: "vk", vk: 0x08, label: "退格" },
    { kind: "vk", vk: 0x09, label: "Tab" },
    { kind: "vk", vk: 0x11, label: "Ctrl" },
    { kind: "vk", vk: 0x10, label: "Shift" },
    { kind: "vk", vk: 0x12, label: "Alt" },
    { kind: "vk", vk: 0x14, label: "Caps" },
    { kind: "vk", vk: 0x5a, label: "Z" },
    { kind: "vk", vk: 0x58, label: "X" },
    { kind: "vk", vk: 0x43, label: "C" },
    { kind: "vk", vk: 0x26, label: "↑" },
    { kind: "vk", vk: 0x28, label: "↓" },
    { kind: "vk", vk: 0x25, label: "←" },
    { kind: "vk", vk: 0x27, label: "→" },
  ],
  alpha: [
    ...Array.from({ length: 26 }, (_, i): KeyBinding => {
      const vk = 0x41 + i;
      return { kind: "vk", vk, label: String.fromCharCode(vk) };
    }),
    ...Array.from({ length: 10 }, (_, i): KeyBinding => {
      const d = (i + 1) % 10;
      return { kind: "vk", vk: 0x30 + d, label: String(d) };
    }),
  ],
  symbol: [
    { kind: "vk", vk: 0xc0, label: "`~" },
    { kind: "vk", vk: 0xbd, label: "-_" },
    { kind: "vk", vk: 0xbb, label: "=+" },
    { kind: "vk", vk: 0xdb, label: "[{" },
    { kind: "vk", vk: 0xdd, label: "]}" },
    { kind: "vk", vk: 0xdc, label: "\\|" },
    { kind: "vk", vk: 0xba, label: ";:" },
    { kind: "vk", vk: 0xde, label: "'\"" },
    { kind: "vk", vk: 0xbc, label: ",<" },
    { kind: "vk", vk: 0xbe, label: ".>" },
    { kind: "vk", vk: 0xbf, label: "/?" },
    { kind: "vk", vk: 0xe2, label: "<>" },
    { kind: "vk", vk: 0x1e2, label: "JIS _" },
    { kind: "vk", vk: 0x1dc, label: "JIS ¥" },
  ],
  fn: [
    ...Array.from({ length: 12 }, (_, i): KeyBinding => ({
      kind: "vk",
      vk: 0x70 + i,
      label: `F${i + 1}`,
    })),
    { kind: "vk", vk: 0x2d, label: "Ins" },
    { kind: "vk", vk: 0x2e, label: "Del" },
    { kind: "vk", vk: 0x24, label: "Home" },
    { kind: "vk", vk: 0x23, label: "End" },
    { kind: "vk", vk: 0x21, label: "PgUp" },
    { kind: "vk", vk: 0x22, label: "PgDn" },
    { kind: "vk", vk: 0x2c, label: "PrtSc" },
    { kind: "vk", vk: 0x91, label: "ScrLk" },
    { kind: "vk", vk: 0x13, label: "Pause" },
    { kind: "vk", vk: 0x5d, label: "Apps" },
    { kind: "vk", vk: 0xa1, label: "RShift" },
    { kind: "vk", vk: 0xa3, label: "RCtrl" },
    { kind: "vk", vk: 0xa5, label: "RAlt" },
    { kind: "vk", vk: 0x1d, label: "無変換" },
    { kind: "vk", vk: 0x1c, label: "変換" },
    { kind: "vk", vk: 0x15, label: "かな" },
    ...Array.from({ length: 12 }, (_, i): KeyBinding => ({
      kind: "vk",
      vk: 0x7c + i,
      label: `F${i + 13}`,
    })),
  ],
  numpad: [
    { kind: "vk", vk: 0x67, label: "Num 7" },
    { kind: "vk", vk: 0x68, label: "Num 8" },
    { kind: "vk", vk: 0x69, label: "Num 9" },
    { kind: "vk", vk: 0x6f, label: "Num /" },
    { kind: "vk", vk: 0x64, label: "Num 4" },
    { kind: "vk", vk: 0x65, label: "Num 5" },
    { kind: "vk", vk: 0x66, label: "Num 6" },
    { kind: "vk", vk: 0x6a, label: "Num *" },
    { kind: "vk", vk: 0x61, label: "Num 1" },
    { kind: "vk", vk: 0x62, label: "Num 2" },
    { kind: "vk", vk: 0x63, label: "Num 3" },
    { kind: "vk", vk: 0x6d, label: "Num -" },
    { kind: "vk", vk: 0x60, label: "Num 0" },
    { kind: "vk", vk: 0x6e, label: "Num ." },
    { kind: "vk", vk: 0x10d, label: "Num ↵" },
    { kind: "vk", vk: 0x6b, label: "Num +" },
    { kind: "vk", vk: 0x90, label: "NumLk" },
    { kind: "vk", vk: 0x6c, label: "Num ," },
  ],
  system: [
    { kind: "sys", sys: "menu", label: "菜单" },
    { kind: "sys", sys: "hide", label: "显隐" },
    { kind: "sys", sys: "kbd", label: "键盘" },
    { kind: "mouse", button: "left", right: false, label: "左键" },
    { kind: "mouse", button: "middle", label: "中键" },
    { kind: "mouse", button: "right", right: true, label: "右键" },
    { kind: "wheel", notches: 1, label: "滚轮 ↑" },
    { kind: "wheel", notches: -1, label: "滚轮 ↓" },
  ],
};

export function bindingEquals(a: KeyBinding, b: KeyBinding): boolean {
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case "vk":
      return b.kind === "vk" && a.vk === b.vk;
    case "mouse":
      return b.kind === "mouse" && mouseButtonOf(a) === mouseButtonOf(b);
    case "wheel":
      return b.kind === "wheel" && a.notches === b.notches;
    case "sys":
      return b.kind === "sys" && a.sys === b.sys;
  }
}

export function bindingCanLatch(binding: KeyBinding): boolean {
  return binding.kind === "vk" || binding.kind === "mouse";
}

export function clampOpacity(value: number): number {
  if (!Number.isFinite(value)) return 0.85;
  return Math.min(MAX_OPACITY, Math.max(MIN_OPACITY, Math.round(value * 100) / 100));
}

export function clampScale(value: number, snap = false): number {
  if (!Number.isFinite(value)) return 1;
  const clamped = Math.min(MAX_SCALE, Math.max(MIN_SCALE, Math.round(value * 100) / 100));
  return snap && Math.abs(clamped - 1) <= 0.02 ? 1 : clamped;
}

export function itemDimensions(item: OverlayItem): { width: number; height: number } {
  const scale = clampScale(item.scale);
  if (item.type === "dpad") {
    const size = Math.round(BASE_DPAD_SIZE * scale);
    return { width: size, height: size };
  }
  const height = Math.round(BASE_KEY_SIZE * scale);
  const wide =
    item.binding.kind !== "sys" &&
    item.binding.label.length >= 3 &&
    !/^[A-Z0-9↑↓←→]$/.test(item.binding.label);
  const width = Math.round((wide ? BASE_KEY_SIZE * 1.26 : BASE_KEY_SIZE) * scale);
  return { width, height };
}

/**
 * Safe rectangle where custom controls may sit (AND-APP-16): clears camera
 * cutouts and rounded corners on all platforms, and gesture strips on iOS.
 */
export function safeBounds(
  layout: GameLayout | null,
  viewportWidth: number,
  viewportHeight: number,
  android: boolean,
): Area {
  if (!layout || layout.width <= 0 || layout.height <= 0) {
    return {
      x: PAD,
      y: PAD,
      width: Math.max(0, viewportWidth - PAD * 2),
      height: Math.max(0, viewportHeight - PAD * 2),
    };
  }
  const { width, height, cutouts, cornerRadius } = layout;
  const gestures = android ? { top: 0, right: 0, bottom: 0, left: 0 } : layout.gestures;
  let x0 = gestures.left;
  let y0 = gestures.top;
  let x1 = width - gestures.right;
  let y1 = height - gestures.bottom;

  for (const c of cutouts) {
    const depths = [c.x + c.width - x0, x1 - c.x, c.y + c.height - y0, y1 - c.y];
    const nearest = depths.indexOf(Math.min(...depths));
    if (nearest === 0) x0 = Math.max(x0, c.x + c.width);
    else if (nearest === 1) x1 = Math.min(x1, c.x);
    else if (nearest === 2) y0 = Math.max(y0, c.y + c.height);
    else y1 = Math.min(y1, c.y);
  }

  const corner = cornerInset(cornerRadius, PAD * 2);
  const inset = Math.max(PAD, Math.ceil(corner * 0.75));
  x0 += inset;
  y0 += inset;
  x1 -= inset;
  y1 -= inset;

  return {
    x: x0,
    y: y0,
    width: Math.max(0, x1 - x0),
    height: Math.max(0, y1 - y0),
  };
}

/** Converts an item's normalized coordinates into clamped viewport pixels. */
export function itemCenter(
  item: OverlayItem,
  bounds: Area,
): { cx: number; cy: number; width: number; height: number } {
  const { width, height } = itemDimensions(item);
  const halfW = width / 2;
  const halfH = height / 2;
  const minX = bounds.x + halfW;
  const maxX = Math.max(minX, bounds.x + bounds.width - halfW);
  const minY = bounds.y + halfH;
  const maxY = Math.max(minY, bounds.y + bounds.height - halfH);
  const rawX = bounds.x + Math.min(1, Math.max(0, item.rx)) * bounds.width;
  const rawY = bounds.y + Math.min(1, Math.max(0, item.ry)) * bounds.height;
  return {
    cx: Math.min(maxX, Math.max(minX, rawX)),
    cy: Math.min(maxY, Math.max(minY, rawY)),
    width,
    height,
  };
}

/** Converts a viewport center point back to normalized coordinates in `bounds`. */
export function pointToNormalized(
  cx: number,
  cy: number,
  item: OverlayItem,
  bounds: Area,
): { rx: number; ry: number } {
  const { width, height } = itemDimensions(item);
  const halfW = width / 2;
  const halfH = height / 2;
  const minX = bounds.x + halfW;
  const maxX = Math.max(minX, bounds.x + bounds.width - halfW);
  const minY = bounds.y + halfH;
  const maxY = Math.max(minY, bounds.y + bounds.height - halfH);
  const clampedX = Math.min(maxX, Math.max(minX, cx));
  const clampedY = Math.min(maxY, Math.max(minY, cy));
  return {
    rx: bounds.width > 0 ? Math.min(1, Math.max(0, (clampedX - bounds.x) / bounds.width)) : 0.5,
    ry: bounds.height > 0 ? Math.min(1, Math.max(0, (clampedY - bounds.y) / bounds.height)) : 0.5,
  };
}

export interface DpadState {
  up: boolean;
  down: boolean;
  left: boolean;
  right: boolean;
}

export const IDLE_DPAD: DpadState = { up: false, down: false, left: false, right: false };

export function dpadDirections(
  dx: number,
  dy: number,
  deadzone: number,
  eightWay: boolean,
): DpadState {
  if (Math.hypot(dx, dy) < deadzone) return IDLE_DPAD;
  if (!eightWay) {
    if (Math.abs(dx) >= Math.abs(dy)) {
      return { up: false, down: false, left: dx < 0, right: dx > 0 };
    }
    return { up: dy < 0, down: dy > 0, left: false, right: false };
  }
  const deg = (Math.atan2(dy, dx) * 180) / Math.PI;
  return {
    right: deg > -67.5 && deg < 67.5,
    down: deg > 22.5 && deg < 157.5,
    left: deg > 112.5 || deg < -112.5,
    up: deg < -22.5 && deg > -157.5,
  };
}

export function dpadVkMap(bind: DpadBind): { up: number; down: number; left: number; right: number } {
  if (bind === "wasd") return { up: 0x57, down: 0x53, left: 0x41, right: 0x44 };
  if (bind === "numpad") return { up: 0x68, down: 0x62, left: 0x64, right: 0x66 };
  return { up: 0x26, down: 0x28, left: 0x25, right: 0x27 };
}

export function defaultProfiles(): KeyProfile[] {
  return [
    {
      id: "rpg",
      name: "RPG",
      builtin: true,
      opacity: 0.85,
      autoDim: true,
      items: [
        { id: "rpg-hide", type: "key", rx: 0.04, ry: 0.08, scale: 0.92, latch: false, binding: { kind: "sys", sys: "hide", label: "显隐" } },
        { id: "rpg-menu", type: "key", rx: 0.96, ry: 0.08, scale: 0.92, latch: false, binding: { kind: "sys", sys: "menu", label: "菜单" } },
        { id: "rpg-shift", type: "key", rx: 0.06, ry: 0.34, scale: 0.96, latch: true, binding: { kind: "vk", vk: 0x10, label: "Shift" } },
        { id: "rpg-dpad", type: "dpad", rx: 0.12, ry: 0.76, scale: 1, bind: "arrows", eightWay: true },
        { id: "rpg-enter", type: "key", rx: 0.93, ry: 0.48, scale: 0.96, latch: false, binding: { kind: "vk", vk: 0x0d, label: "Enter" } },
        { id: "rpg-esc", type: "key", rx: 0.82, ry: 0.56, scale: 0.96, latch: false, binding: { kind: "vk", vk: 0x1b, label: "Esc" } },
        { id: "rpg-x", type: "key", rx: 0.81, ry: 0.83, scale: 1.04, latch: false, binding: { kind: "vk", vk: 0x58, label: "X" } },
        { id: "rpg-z", type: "key", rx: 0.92, ry: 0.75, scale: 1.12, latch: false, binding: { kind: "vk", vk: 0x5a, label: "Z" } },
      ],
    },
    {
      id: "shortcuts",
      name: "快捷键",
      builtin: true,
      opacity: 0.82,
      autoDim: true,
      items: [
        { id: "sc-hide", type: "key", rx: 0.04, ry: 0.08, scale: 0.92, latch: false, binding: { kind: "sys", sys: "hide", label: "显隐" } },
        { id: "sc-menu", type: "key", rx: 0.96, ry: 0.08, scale: 0.92, latch: false, binding: { kind: "sys", sys: "menu", label: "菜单" } },
        { id: "sc-ctrl", type: "key", rx: 0.06, ry: 0.82, scale: 1, latch: false, binding: { kind: "vk", vk: 0x11, label: "Ctrl" } },
        { id: "sc-esc", type: "key", rx: 0.94, ry: 0.36, scale: 0.96, latch: false, binding: { kind: "vk", vk: 0x1b, label: "Esc" } },
        { id: "sc-rmb", type: "key", rx: 0.94, ry: 0.58, scale: 0.96, latch: false, binding: { kind: "mouse", right: true, label: "右键" } },
        { id: "sc-space", type: "key", rx: 0.93, ry: 0.82, scale: 1.06, latch: false, binding: { kind: "vk", vk: 0x20, label: "Space" } },
      ],
    },
  ];
}

function validBinding(raw: unknown): KeyBinding | null {
  if (!raw || typeof raw !== "object") return null;
  const b = raw as Record<string, unknown>;
  if (typeof b.label !== "string" || !b.label) return null;
  if (b.kind === "vk" && typeof b.vk === "number" && Number.isInteger(b.vk) && b.vk > 0 && b.vk <= 0x1ff) {
    return { kind: "vk", vk: b.vk, label: b.label };
  }
  if (b.kind === "mouse") {
    if (b.button === "left" || b.button === "middle" || b.button === "right") {
      return { kind: "mouse", button: b.button, right: b.button === "right", label: b.label };
    }
    if (typeof b.right === "boolean") {
      return { kind: "mouse", button: b.right ? "right" : "left", right: b.right, label: b.label };
    }
  }
  if (b.kind === "wheel" && (b.notches === 1 || b.notches === -1)) {
    return { kind: "wheel", notches: b.notches, label: b.label };
  }
  if (b.kind === "sys" && (b.sys === "menu" || b.sys === "hide" || b.sys === "kbd")) {
    return { kind: "sys", sys: b.sys, label: b.label };
  }
  return null;
}

function validItem(raw: unknown, fallbackIdx: number): OverlayItem | null {
  if (!raw || typeof raw !== "object") return null;
  const it = raw as Record<string, unknown>;
  const id = typeof it.id === "string" && it.id ? it.id : `item-${fallbackIdx}`;
  const rx = typeof it.rx === "number" && Number.isFinite(it.rx) ? Math.min(1, Math.max(0, it.rx)) : 0.5;
  const ry = typeof it.ry === "number" && Number.isFinite(it.ry) ? Math.min(1, Math.max(0, it.ry)) : 0.5;
  const scale = clampScale(typeof it.scale === "number" ? it.scale : 1);

  if (it.type === "dpad") {
    return {
      id,
      type: "dpad",
      rx,
      ry,
      scale,
      bind: it.bind === "wasd" ? "wasd" : it.bind === "numpad" ? "numpad" : "arrows",
      eightWay: it.eightWay !== false,
    };
  }
  if (it.type === "key") {
    const binding = validBinding(it.binding);
    if (!binding) return null;
    return {
      id,
      type: "key",
      rx,
      ry,
      scale,
      latch: Boolean(it.latch) && bindingCanLatch(binding),
      binding,
    };
  }
  return null;
}

export function normalizeStoredKeyLayouts(raw: unknown): StoredKeyLayouts {
  const defaults = defaultProfiles();
  if (!raw || typeof raw !== "object") {
    return { profiles: defaults, games: {} };
  }
  const obj = raw as Record<string, unknown>;
  const profiles: KeyProfile[] = [];
  const seenIds = new Set<string>();

  if (Array.isArray(obj.profiles)) {
    for (const entry of obj.profiles) {
      if (!entry || typeof entry !== "object") continue;
      const p = entry as Record<string, unknown>;
      if (typeof p.id !== "string" || !p.id || seenIds.has(p.id)) continue;
      if (typeof p.name !== "string" || !p.name.trim()) continue;
      const items = Array.isArray(p.items)
        ? p.items.map((item, i) => validItem(item, i)).filter((v): v is OverlayItem => v !== null)
        : [];
      seenIds.add(p.id);
      profiles.push({
        id: p.id,
        name: p.name.trim().slice(0, 24),
        builtin: Boolean(p.builtin),
        opacity: clampOpacity(typeof p.opacity === "number" ? p.opacity : 0.85),
        autoDim: p.autoDim !== false,
        items,
      });
    }
  }

  for (const builtin of defaults) {
    if (!seenIds.has(builtin.id)) {
      profiles.unshift(builtin);
      seenIds.add(builtin.id);
    }
  }

  const games: Record<string, string> = {};
  if (obj.games && typeof obj.games === "object") {
    for (const [gameId, profileId] of Object.entries(obj.games as Record<string, unknown>)) {
      if (typeof profileId === "string" && seenIds.has(profileId)) {
        games[gameId] = profileId;
      }
    }
  }
  return { profiles, games };
}
