import type { Action } from "svelte/action";

/** A touch hold opens a card menu without also activating its click. */
export const longPress: Action<HTMLButtonElement, (() => void) | null> = (node, action) => {
  let press: { id: number; x: number; y: number; timer: ReturnType<typeof setTimeout> } | null = null;
  let held = false;
  const cancel = () => {
    if (!press) return;
    clearTimeout(press.timer);
    press = null;
  };
  const down = (event: PointerEvent) => {
    if (!event.isPrimary || event.button !== 0) return;
    held = false;
    if (!action || event.pointerType === "pen") return;
    cancel();
    press = {
      id: event.pointerId, x: event.clientX, y: event.clientY,
      timer: setTimeout(() => {
        press = null;
        held = true;
        action?.();
      }, 500),
    };
  };
  const move = (event: PointerEvent) => {
    if (press?.id === event.pointerId && Math.hypot(event.clientX - press.x, event.clientY - press.y) > 10) cancel();
  };
  const end = (event: PointerEvent) => {
    if (press?.id === event.pointerId) cancel();
  };
  const click = (event: MouseEvent) => {
    if (!held || event.detail === 0) return;
    held = false;
    event.preventDefault();
    event.stopImmediatePropagation();
  };
  const context = (event: MouseEvent) => { if (action) event.preventDefault(); };
  const pointer = [
    ["pointerdown", down], ["pointermove", move], ["pointerup", end], ["pointercancel", end], ["pointerleave", end],
  ] as const;
  for (const [name, handler] of pointer) node.addEventListener(name, handler);
  node.addEventListener("click", click, true);
  node.addEventListener("contextmenu", context);
  return {
    update(next) { action = next; if (!action) cancel(); },
    destroy() {
      cancel();
      for (const [name, handler] of pointer) node.removeEventListener(name, handler);
      node.removeEventListener("click", click, true);
      node.removeEventListener("contextmenu", context);
    },
  };
};
