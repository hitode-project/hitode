import type { Action } from "svelte/action";

type Point = { x: number; y: number };

export interface Pinch {
  /** Off: pinches pass through untouched. */
  enabled: boolean;
  /** The gesture so far: `scale` against its start, which was at `at` on screen. */
  onpinch: (scale: number, at: Point) => void;
  /** The fingers lifted. */
  onend: () => void;
}

/** Ctrl/⌘ + wheel has no end event; a pause this long ends it. */
const WHEEL_END_MS = 160;
/** Two fingers on a screen pinch only once their distance changed this much (px)… */
const TOUCH_SLOP = 24;
/** …and then scale the covers by this power of their spread, a little less than one to one. */
const TOUCH_GAIN = 0.75;

/**
 * Continuous pinches over the library grid: WebKit's trackpad gesture
 * events (macOS, iOS), two-finger touches (Android, iOS), and Ctrl/⌘ + wheel,
 * which Chromium sends for a trackpad pinch and a mouse sends with the key
 * held. One source owns a gesture until it ends.
 *
 * A second finger is often an accident: two fingers on a screen pinch only
 * after their distance changes by `TOUCH_SLOP`, and not at all when they
 * land on a list that is already scrolling.
 */
export const gridZoom: Action<HTMLElement, Pinch> = (node, initial) => {
  let pinch = initial;
  let gesture: { source: "wheel" | "webkit" | "touch"; at: Point; scale: number; span: number; armed: boolean; scroll: boolean } | null = null;
  let wheelTimer: ReturnType<typeof setTimeout> | undefined;

  function start(source: "wheel" | "webkit" | "touch", at: Point, span = 0): boolean {
    if (!pinch.enabled || (gesture && gesture.source !== source)) return false;
    gesture ??= { source, at, scale: 1, span, armed: source !== "touch", scroll: false };
    return true;
  }

  function update(scale: number) {
    if (!gesture) return;
    gesture.scale = scale;
    pinch.onpinch(scale, gesture.at);
  }

  function end() {
    if (!gesture) return;
    const { armed } = gesture;
    gesture = null;
    if (armed) pinch.onend();
  }

  function onwheel(event: WheelEvent) {
    if ((!event.ctrlKey && !event.metaKey) || !start("wheel", { x: event.clientX, y: event.clientY })) return;
    event.preventDefault();
    update(gesture!.scale * Math.exp(-Math.max(-50, Math.min(50, event.deltaY)) / 100));
    clearTimeout(wheelTimer);
    wheelTimer = setTimeout(end, WHEEL_END_MS);
  }

  // WebKit GestureEvent: `scale` is already relative to the gesture start.
  type GestureEvent = UIEvent & { scale: number; clientX: number; clientY: number };
  function ongesturestart(event: Event) {
    const e = event as GestureEvent;
    if (start("webkit", { x: e.clientX, y: e.clientY })) event.preventDefault();
  }
  function ongesturechange(event: Event) {
    if (gesture?.source !== "webkit") return;
    event.preventDefault();
    update((event as GestureEvent).scale);
  }
  function ongestureend(event: Event) {
    if (gesture?.source !== "webkit") return;
    event.preventDefault();
    end();
  }

  const span = (touches: TouchList) =>
    Math.hypot(touches[0]!.clientX - touches[1]!.clientX, touches[0]!.clientY - touches[1]!.clientY);
  const middle = (touches: TouchList) =>
    ({ x: (touches[0]!.clientX + touches[1]!.clientX) / 2, y: (touches[0]!.clientY + touches[1]!.clientY) / 2 });

  function ontouchstart(event: TouchEvent) {
    if (event.touches.length !== 2) return;
    start("touch", middle(event.touches), span(event.touches));
  }
  function ontouchmove(event: TouchEvent) {
    if (gesture?.source !== "touch" || gesture.scroll || event.touches.length !== 2) return;
    // The list already scrolls, and the browser keeps these fingers for it.
    if (!event.cancelable) return void (gesture.scroll = true);
    // Held from the first move: Chromium keeps a gesture it saw move unhindered.
    event.preventDefault();
    const now = span(event.touches);
    if (!gesture.armed) {
      if (Math.abs(now - gesture.span) < TOUCH_SLOP) return;
      Object.assign(gesture, { armed: true, span: now, at: middle(event.touches) });
    }
    update((now / gesture.span) ** TOUCH_GAIN);
  }
  function ontouchend(event: TouchEvent) {
    if (gesture?.source === "touch" && event.touches.length < 2) end();
  }

  const listeners = [
    ["wheel", onwheel, { passive: false }],
    ["gesturestart", ongesturestart, { passive: false }],
    ["gesturechange", ongesturechange, { passive: false }],
    ["gestureend", ongestureend, { passive: false }],
    ["touchstart", ontouchstart, { passive: true }],
    ["touchmove", ontouchmove, { passive: false }],
    ["touchend", ontouchend, { passive: true }],
    ["touchcancel", ontouchend, { passive: true }],
  ] as const;
  for (const [name, handler, options] of listeners) node.addEventListener(name, handler as EventListener, options);
  return {
    update(next) { pinch = next; },
    destroy() {
      clearTimeout(wheelTimer);
      for (const [name, handler] of listeners) node.removeEventListener(name, handler as EventListener);
    },
  };
};
