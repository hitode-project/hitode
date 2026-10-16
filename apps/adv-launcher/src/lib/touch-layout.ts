import type { Area, GameLayout } from "./screen.svelte";

/**
 * Where the touch controls go around a mobile game (AND-APP-16): the side
 * letterboxes on a phone, the top and bottom ones on a tablet, or glass
 * panels over the game when neither leaves room. Controls may use the
 * letterbox up to the screen edge but never a cutout, a rounded corner or a
 * system gesture edge. All values are CSS pixels of the page.
 */
export type Arrangement =
  | { kind: "side"; wide: boolean; left: Area; right: Area }
  | { kind: "band"; top: Area; bottom: Area }
  | { kind: "float"; inner: Area };

/** Space between a control group and the letterbox's edges. */
const PAD = 6;
/** Two columns of keys with labels. */
const WIDE = 128;
/** One column of icons and short keys. */
const NARROW = 32;
/** Space kept beside the game. */
const BESIDE_GAME = 4;
/** Space kept beside a cutout, which has no rounded glass of its own to clear. */
const BESIDE_CUTOUT = 2;
const KEY = 44;

/** How far a rounded corner of `radius` cuts in at `depth` from its edges. */
export function cornerInset(radius: number, depth: number): number {
  if (depth >= radius) return 0;
  const d = radius - depth;
  return radius - Math.sqrt(radius * radius - d * d);
}

function overlaps(a0: number, a1: number, b0: number, b1: number): boolean {
  return a0 < b1 && b0 < a1;
}

function side(layout: GameLayout, which: "left" | "right"): Area {
  const { width, height, game, cutouts, cornerRadius, gestures } = layout;
  let x0 = which === "left" ? 0 : game.x + game.width;
  let x1 = which === "left" ? game.x : width;
  // A cutout in the letterbox pushes the controls past it.
  let [outer0, outer1] = which === "left" ? [PAD, BESIDE_GAME] : [BESIDE_GAME, PAD];
  for (const c of cutouts) {
    if (!overlaps(x0, x1, c.x, c.x + c.width)) continue;
    if (which === "left") [x0, outer0] = [Math.max(x0, c.x + c.width), BESIDE_CUTOUT];
    else [x1, outer1] = [Math.min(x1, c.x), BESIDE_CUTOUT];
  }
  x0 += outer0;
  x1 -= outer1;
  const corner = cornerInset(cornerRadius, which === "left" ? x0 : width - x1);
  const top = Math.max(PAD, gestures.top, corner);
  const bottom = Math.max(PAD, gestures.bottom, corner);
  return { x: x0, y: top, width: Math.max(0, x1 - x0), height: Math.max(0, height - top - bottom) };
}

function band(layout: GameLayout, which: "top" | "bottom"): Area {
  const { width, height, game, cutouts, cornerRadius, gestures } = layout;
  let y0 = which === "top" ? 0 : game.y + game.height;
  let y1 = which === "top" ? game.y : height;
  for (const c of cutouts) {
    if (!overlaps(y0, y1, c.y, c.y + c.height)) continue;
    if (which === "top") y0 = Math.max(y0, c.y + c.height);
    else y1 = Math.min(y1, c.y);
  }
  y0 = Math.max(y0, which === "top" ? gestures.top : 0) + PAD;
  y1 = Math.min(y1, height - gestures.bottom) - PAD;
  const corner = cornerInset(cornerRadius, which === "top" ? y0 : height - y1);
  const inset = Math.max(PAD * 2, gestures.left, gestures.right, corner);
  return { x: inset, y: y0, width: Math.max(0, width - 2 * inset), height: Math.max(0, y1 - y0) };
}

/** The screen less its cutouts, corners and gesture edges, for floating panels. */
function inner(layout: GameLayout): Area {
  const { width, height, cutouts, cornerRadius, gestures } = layout;
  let [x0, y0, x1, y1] = [gestures.left, gestures.top, width - gestures.right, height - gestures.bottom];
  for (const c of cutouts) {
    // Each cutout clears the nearest edge.
    const depths = [c.x + c.width - x0, x1 - c.x, c.y + c.height - y0, y1 - c.y];
    const nearest = depths.indexOf(Math.min(...depths));
    if (nearest === 0) x0 = Math.max(x0, c.x + c.width);
    else if (nearest === 1) x1 = Math.min(x1, c.x);
    else if (nearest === 2) y0 = Math.max(y0, c.y + c.height);
    else y1 = Math.min(y1, c.y);
  }
  // A panel's corner sits about a key's height in from the screen's corner.
  const corner = cornerInset(cornerRadius, PAD * 2);
  const inset = Math.max(PAD * 2, corner * 0.7);
  return { x: x0 + inset, y: y0 + inset, width: x1 - x0 - 2 * inset, height: y1 - y0 - 2 * inset };
}

export function arrange(layout: GameLayout): Arrangement {
  const left = side(layout, "left");
  const right = side(layout, "right");
  const columns = Math.min(left.width, right.width);
  if (columns >= NARROW && Math.min(left.height, right.height) >= KEY * 4) {
    return { kind: "side", wide: columns >= WIDE, left, right };
  }
  const top = band(layout, "top");
  const bottom = band(layout, "bottom");
  if (Math.min(top.height, bottom.height) >= KEY) return { kind: "band", top, bottom };
  return { kind: "float", inner: inner(layout) };
}

/**
 * The key height that fits `count` keys with `fixed` other content into
 * `height`, between 32 and 44 pixels.
 */
export function keyHeight(height: number, count: number, fixed: number, gap = 6): number {
  const fit = (height - fixed - gap * count) / count;
  return Math.max(32, Math.min(KEY, Math.floor(fit)));
}
