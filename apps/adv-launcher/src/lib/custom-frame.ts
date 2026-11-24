import type { Area, GameLayout } from "./screen.svelte";

/** Target aspect ratio bounds (`width / height * 1000`), mirroring `adv_display::plan`. */
export const CUSTOM_ASPECT_MIN = 500;
export const CUSTOM_ASPECT_MAX = 3000;
/** Scale bounds in thousandths of the fitted size (`50%..=100%`). */
export const CUSTOM_ZOOM_MIN = 500;
export const CUSTOM_ZOOM_MAX = 1000;
export const DEFAULT_CUSTOM_ASPECT = 1778;
export const DEFAULT_CUSTOM_ZOOM = 1000;

export interface CustomFrame {
  aspect: number;
  zoom: number;
}

export const ASPECT_PRESETS: { label: string; aspect: number }[] = [
  { label: "4:3", aspect: 1333 },
  { label: "16:10", aspect: 1600 },
  { label: "16:9", aspect: 1778 },
];

export function clampAspect(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_CUSTOM_ASPECT;
  return Math.max(CUSTOM_ASPECT_MIN, Math.min(CUSTOM_ASPECT_MAX, Math.round(value)));
}

export function clampZoom(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_CUSTOM_ZOOM;
  return Math.max(CUSTOM_ZOOM_MIN, Math.min(CUSTOM_ZOOM_MAX, Math.round(value)));
}

/**
 * Largest centered area no camera cutout reaches, matching
 * `adv_shell::mobile::Occlusion::content` (AND-APP-16).
 */
export function contentArea(layout: GameLayout): Area {
  const { width, height, cutouts } = layout;
  let x = 0;
  let y = 0;
  for (const cutout of cutouts) {
    let raw = cutout.x + cutout.width;
    let horizontal = true;
    for (const [d, h] of [
      [width - cutout.x, true],
      [cutout.y + cutout.height, false],
      [height - cutout.y, false],
    ] as const) {
      if (d < raw) {
        raw = d;
        horizontal = h;
      }
    }
    const depth = Math.max(0, Math.min(raw, (horizontal ? width : height) / 2));
    if (horizontal) x = Math.max(x, depth);
    else y = Math.max(y, depth);
  }
  return {
    x,
    y,
    width: Math.max(0, width - 2 * x),
    height: Math.max(0, height - 2 * y),
  };
}

/** Mirrors `adv_display::plan::Placement::Custom::rectangle` inside `area`. */
export function customRect(area: Area, aspect: number, zoom: number): Area {
  const cw = area.width;
  const ch = area.height;
  if (cw <= 0 || ch <= 0) {
    return { x: area.x, y: area.y, width: 0, height: 0 };
  }
  const r = clampAspect(aspect) / 1000;
  const z = clampZoom(zoom) / 1000;
  const [fitW, fitH] = cw / ch >= r ? [ch * r, ch] : [cw, cw / r];
  const width = Math.max(1, Math.min(cw, Math.floor(fitW * z)));
  const height = Math.max(1, Math.min(ch, Math.floor(fitH * z)));
  return {
    x: area.x + Math.floor((cw - width) / 2),
    y: area.y + Math.floor((ch - height) / 2),
    width,
    height,
  };
}

/**
 * Inverse of `customRect`: maps any centered `(width, height)` inside `area`
 * to its `{ aspect, zoom }` representation.
 */
export function rectToCustom(area: Area, width: number, height: number): CustomFrame {
  const cw = Math.max(1, area.width);
  const ch = Math.max(1, area.height);
  const w = Math.max(1, Math.min(cw, width));
  const h = Math.max(1, Math.min(ch, height));
  const aspect = clampAspect((w / h) * 1000);
  const r = aspect / 1000;
  const [fitW, fitH] = cw / ch >= r ? [ch * r, ch] : [cw, cw / r];
  const zoom = clampZoom(Math.max(w / fitW, h / fitH) * 1000);
  return { aspect, zoom };
}

/** Human-readable aspect label, snapping close to standard ratios. */
export function formatAspect(aspect: number): string {
  const value = clampAspect(aspect);
  const known: [number, string][] = [
    [1000, "1:1"],
    [1250, "5:4"],
    [1333, "4:3"],
    [1500, "3:2"],
    [1600, "16:10"],
    [1778, "16:9"],
    [2000, "2:1"],
    [2167, "19.5:9"],
    [2333, "21:9"],
  ];
  for (const [target, label] of known) {
    if (Math.abs(value - target) <= 12) return label;
  }
  return `${(value / 1000).toFixed(2)}:1`;
}

export function formatZoom(zoom: number): string {
  return `${Math.round(clampZoom(zoom) / 10)}%`;
}
