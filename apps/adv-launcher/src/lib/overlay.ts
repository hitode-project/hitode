import { call } from "./bridge";

/**
 * Publishes what the in-game overlay claims of the window's input, so the
 * native host view can route each pointer press and the keyboard between the
 * game view underneath and this webview. The routing rules live in Rust
 * (adv_shell::overlay); this module only describes the overlay:
 *
 * - `regions`: the viewport rectangles of elements marked with
 *   `{@attach region}`, in CSS pixels from the top-left corner;
 * - `modal`: some element marked with `{@attach modal}` is visible, so every
 *   press belongs to the overlay;
 * - `wantsKeyboard`: a modal layer is open or an editable control has focus;
 * - `visible`: the overlay draws something over the game: a region, a modal
 *   layer or an element marked with `{@attach shown}`. While it is false the
 *   host may hide the webview, which also stops its animation frames.
 *
 * Outside the game screen the overlay claims everything. Updates are batched
 * per animation frame and only sent when the description changes.
 */

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface OverlayInput {
  regions: Rect[];
  modal: boolean;
  wantsKeyboard: boolean;
  visible: boolean;
}

const EXCLUSIVE: OverlayInput = { regions: [], modal: true, wantsKeyboard: true, visible: true };

const regions = new Set<Element>();
const modals = new Set<Element>();
const shownElements = new Set<Element>();
let gameActive = false;
let frame = 0;
let lastSent = "";
let listening = false;
const listeners = new Set<(input: OverlayInput) => void>();

const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => schedule());

/** Marks an element as interactive overlay UI: presses inside it go to the webview. */
export function region(node: Element): () => void {
  regions.add(node);
  resize?.observe(node);
  schedule();
  return () => {
    regions.delete(node);
    resize?.unobserve(node);
    schedule();
  };
}

/** Marks a layer that covers the game: while it is visible, every press and the keyboard go to the webview. */
export function modal(node: Element): () => void {
  modals.add(node);
  schedule();
  return () => {
    modals.delete(node);
    schedule();
  };
}

/** Marks non-interactive overlay content, such as the FPS readout, that keeps the webview shown. */
export function shown(node: Element): () => void {
  shownElements.add(node);
  schedule();
  return () => {
    shownElements.delete(node);
    schedule();
  };
}

/** Switches between the game screen (regions apply) and every other screen (the webview takes all input). */
export function setGameActive(active: boolean): void {
  gameActive = active;
  listen();
  schedule();
}

/** Observes each published description; used by the diagnostic region outline. */
export function onOverlayInput(listener: (input: OverlayInput) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function visible(element: Element): boolean {
  if (!element.isConnected) return false;
  return typeof element.checkVisibility === "function"
    ? element.checkVisibility({ visibilityProperty: true })
    : true;
}

function editable(element: Element | null): boolean {
  if (!(element instanceof HTMLElement)) return false;
  if (element.isContentEditable) return true;
  if (element instanceof HTMLTextAreaElement) return !element.readOnly && !element.disabled;
  if (element instanceof HTMLInputElement) {
    const textual = ["text", "search", "url", "email", "password", "tel", "number"];
    return textual.includes(element.type) && !element.readOnly && !element.disabled;
  }
  return false;
}

/** Viewport rectangle of `element`, grown outwards to whole pixels and clipped to the viewport. */
export function pixelRect(r: DOMRectReadOnly, viewWidth: number, viewHeight: number): Rect | null {
  const left = Math.max(0, Math.floor(r.left));
  const top = Math.max(0, Math.floor(r.top));
  const right = Math.min(viewWidth, Math.ceil(r.right));
  const bottom = Math.min(viewHeight, Math.ceil(r.bottom));
  if (right <= left || bottom <= top) return null;
  return { x: left, y: top, width: right - left, height: bottom - top };
}

export function describe(): OverlayInput {
  if (!gameActive) return EXCLUSIVE;
  const rects: Rect[] = [];
  for (const element of regions) {
    if (!visible(element)) continue;
    const rect = pixelRect(element.getBoundingClientRect(), window.innerWidth, window.innerHeight);
    if (rect) rects.push(rect);
  }
  rects.sort((a, b) => a.y - b.y || a.x - b.x || a.width - b.width || a.height - b.height);
  const isModal = [...modals].some(visible);
  return {
    regions: rects,
    modal: isModal,
    wantsKeyboard: isModal || editable(document.activeElement),
    visible: rects.length > 0 || isModal || [...shownElements].some(visible),
  };
}

function schedule(): void {
  if (frame || typeof requestAnimationFrame === "undefined") return;
  frame = requestAnimationFrame(flush);
}

function flush(): void {
  frame = 0;
  const input = describe();
  const text = JSON.stringify(input);
  if (text !== lastSent) {
    lastSent = text;
    for (const listener of listeners) listener(input);
    void call("set_overlay_input", { input }, null).catch((error: unknown) => {
      console.error("set_overlay_input failed", error);
      lastSent = "";
    });
  }
  // Regions move with transitions and animations without resizing; follow
  // them frame by frame until the finite ones settle.
  if (gameActive && settling()) schedule();
}

function settling(): boolean {
  return document.getAnimations().some((animation) => {
    if (animation.playState !== "running") return false;
    const end = animation.effect?.getComputedTiming().endTime;
    return typeof end === "number" && Number.isFinite(end);
  });
}

function listen(): void {
  if (listening) return;
  listening = true;
  const update = () => schedule();
  window.addEventListener("resize", update);
  document.addEventListener("scroll", update, { capture: true, passive: true });
  document.addEventListener("focusin", update);
  document.addEventListener("focusout", update);
  document.addEventListener("transitionrun", update);
  document.addEventListener("animationstart", update);
  new MutationObserver(update).observe(document.documentElement, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ["class", "style", "hidden", "open"],
  });
}
