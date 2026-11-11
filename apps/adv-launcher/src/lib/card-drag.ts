import { tick } from "svelte";
import type { Action } from "svelte/action";
import { moveTo } from "./arrange";
import { REFLOW, measure, play } from "./reflow";

export interface CardDragOptions {
  /** Cards can be dragged to a new place (not while searching). */
  movable: boolean;
  /** A touch hold released in place opens the card's menu; null leaves holds alone. */
  onhold: ((id: string) => void) | null;
  /** The order to show while a card is dragged; null ends the preview. */
  onpreview: (ids: string[] | null) => void;
  /** A drag or Alt+arrow put `id` at a new place in `ids`, the visible order. */
  onmove: (ids: string[], id: string) => void;
  /** The card Alt+arrow moves when no card has focus (WebKit does not focus clicked buttons). */
  selected?: string | null;
}

const HOLD_MS = 350;
const MOUSE_SLOP = 4;
const TOUCH_SLOP = 8;
const EDGE = 56;
/** A slot must be this share of a card width nearer than the current one to take the card. */
const STICK = 0.2;

/**
 * Pointer and keyboard arrangement of a library grid whose children are cards
 * with `data-id`, inside its scrolling parent. A mouse drags after a few
 * pixels; a touch lifts the card after a hold, then drags it or, released in
 * place, opens its menu. While dragging, the other cards make room with FLIP
 * and the list scrolls near its edges. The grid must be `position: relative`.
 *
 * The page styles the classes it sets: `lifted` and `dragging` raise the card
 * (with the `scale` property, so it eases in), `settling` keeps it above its
 * neighbours until it lands. The pointer moves it with `translate`, which the
 * FLIP `transform` of reflow.ts composes with.
 */
export const cardDrag: Action<HTMLElement, CardDragOptions> = (grid, initial) => {
  let options = initial;
  const scroller = grid.parentElement!;
  let press: { pointer: number; card: HTMLElement; x: number; y: number; touch: boolean; timer: ReturnType<typeof setTimeout> | null; lifted: boolean } | null = null;
  let drag: {
    pointer: number; card: HTMLElement; id: string; start: string; ids: string[]; slots: { x: number; y: number }[];
    grab: { x: number; y: number }; stick: number; x: number; y: number; frame: number;
  } | null = null;
  let swallowClick = false;

  const cardOf = (target: EventTarget | null) =>
    target instanceof Element ? (target.closest("[data-id]") as HTMLElement | null) : null;

  function lift(card: HTMLElement, on: boolean) {
    card.classList.toggle("lifted", on);
  }

  function endPress() {
    if (!press) return;
    if (press.timer) clearTimeout(press.timer);
    if (!drag) lift(press.card, false);
    press = null;
  }

  function down(event: PointerEvent) {
    if (drag) return;
    // A second finger is a pinch, not a hold.
    if (!event.isPrimary) return endPress();
    if (event.button !== 0) return;
    swallowClick = false;
    const card = cardOf(event.target);
    if (!card) return;
    const touch = event.pointerType !== "mouse";
    const { onhold, movable } = options;
    if (touch ? !onhold && !movable : !movable) return;
    press = { pointer: event.pointerId, card, x: event.clientX, y: event.clientY, touch, timer: null, lifted: false };
    if (touch) {
      press.timer = setTimeout(() => {
        if (!press) return;
        press.timer = null;
        press.lifted = true;
        lift(press.card, true);
      }, HOLD_MS);
    }
  }

  function move(event: PointerEvent) {
    if (drag?.pointer === event.pointerId) {
      drag.x = event.clientX;
      drag.y = event.clientY;
      return;
    }
    if (press?.pointer !== event.pointerId) return;
    const distance = Math.hypot(event.clientX - press.x, event.clientY - press.y);
    if (press.touch && !press.lifted) {
      if (distance > TOUCH_SLOP) endPress();
    } else if (distance > (press.touch ? TOUCH_SLOP : MOUSE_SLOP) && options.movable) {
      begin(event);
    }
  }

  function up(event: PointerEvent) {
    if (drag?.pointer === event.pointerId) return void finish(event.type === "pointerup");
    if (press?.pointer !== event.pointerId) return;
    const held = press.lifted && event.type === "pointerup" ? press.card.dataset.id : undefined;
    endPress();
    if (held !== undefined) {
      swallowClick = true;
      options.onhold?.(held);
    }
  }

  function begin(event: PointerEvent) {
    const { card } = press!;
    const cards = [...grid.children] as HTMLElement[];
    const box = grid.getBoundingClientRect();
    drag = {
      pointer: press!.pointer, card, id: card.dataset.id!,
      ids: cards.map((c) => c.dataset.id!),
      start: cards.map((c) => c.dataset.id!).join(),
      slots: cards.map((c) => ({ x: c.offsetLeft, y: c.offsetTop })),
      grab: { x: press!.x - box.left - card.offsetLeft, y: press!.y - box.top - card.offsetTop },
      stick: card.offsetWidth * STICK,
      x: event.clientX, y: event.clientY, frame: 0,
    };
    endPress();
    swallowClick = true;
    grid.setPointerCapture(drag.pointer);
    for (const running of card.getAnimations({ subtree: true })) if (running.id === REFLOW) running.cancel();
    card.classList.add("dragging");
    grid.classList.add("arranging");
    drag.frame = requestAnimationFrame(frame);
  }

  /** Once per frame: scroll near an edge, follow the pointer, find the slot under the card. */
  function frame() {
    if (!drag) return;
    const view = scroller.getBoundingClientRect();
    const speed = drag.y < view.top + EDGE ? drag.y - (view.top + EDGE) : drag.y > view.bottom - EDGE ? drag.y - (view.bottom - EDGE) : 0;
    if (speed) scroller.scrollTop += Math.max(-1, Math.min(1, speed / EDGE)) * 16;
    const box = grid.getBoundingClientRect();
    const x = drag.x - box.left - drag.grab.x;
    const y = drag.y - box.top - drag.grab.y;
    // The nearest slot, with the current one favoured, so a card held on
    // the line between two slots does not make its neighbours flip back and forth.
    const distance = (i: number) => Math.hypot(drag!.slots[i]!.x - x, drag!.slots[i]!.y - y);
    let index = drag.ids.indexOf(drag.id);
    let best = distance(index) - drag.stick;
    drag.slots.forEach((_, i) => {
      const d = distance(i);
      if (d < best) [index, best] = [i, d];
    });
    if (drag.ids[index] !== drag.id) {
      const before = measure(grid);
      drag.ids = moveTo(drag.ids, drag.id, index);
      options.onpreview(drag.ids);
      const card = drag.card;
      void tick().then(() => play(grid, before, { skip: card }));
    }
    const slot = drag.slots[drag.ids.indexOf(drag.id)]!;
    drag.card.style.translate = `${x - slot.x}px ${y - slot.y}px`;
    drag.frame = requestAnimationFrame(frame);
  }

  /** Drops the card where it is shown (or puts everything back) and settles it into its slot. */
  async function finish(commit: boolean) {
    if (!drag) return;
    const { card, id, ids, start, frame: pending, pointer } = drag;
    drag = null;
    cancelAnimationFrame(pending);
    if (grid.hasPointerCapture(pointer)) grid.releasePointerCapture(pointer);
    const before = measure(grid);
    card.style.translate = "";
    card.classList.remove("dragging", "lifted");
    card.classList.add("settling");
    grid.classList.remove("arranging");
    options.onpreview(null);
    if (commit && ids.join() !== start) options.onmove(ids, id);
    await tick();
    play(grid, before);
    await Promise.allSettled(card.getAnimations().map((a) => a.finished));
    card.classList.remove("settling");
  }

  function key(event: KeyboardEvent) {
    if (!event.altKey || !options.movable || drag || !event.key.startsWith("Arrow")) return;
    const target = event.target as HTMLElement;
    if (target.closest("input, select, textarea, [contenteditable], [role=dialog]")) return;
    const focused = cardOf(target);
    const card = focused && grid.contains(focused)
      ? focused
      : options.selected ? (grid.querySelector(`[data-id="${CSS.escape(options.selected)}"]`) as HTMLElement | null) : null;
    if (!card) return;
    const columns = getComputedStyle(grid).gridTemplateColumns.split(" ").length;
    const step = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -columns, ArrowDown: columns }[event.key];
    if (!step) return;
    event.preventDefault();
    const ids = [...grid.children].map((c) => (c as HTMLElement).dataset.id!);
    const id = card.dataset.id!;
    const index = Math.max(0, Math.min(ids.length - 1, ids.indexOf(id) + step));
    if (ids[index] === id) return;
    const before = measure(grid);
    options.onmove(moveTo(ids, id, index), id);
    void tick().then(() => {
      play(grid, before);
      // Moving a node drops its focus.
      if (focused === card) card.focus({ preventScroll: true });
      card.scrollIntoView({ block: "nearest", behavior: "smooth" });
    });
  }

  function click(event: MouseEvent) {
    if (!swallowClick || event.detail === 0) return;
    swallowClick = false;
    event.preventDefault();
    event.stopImmediatePropagation();
  }

  /** A lifted or dragged card keeps the finger; the list must not scroll under it. */
  function touchmove(event: TouchEvent) {
    if (drag || press?.lifted) event.preventDefault();
  }

  /**
   * A touch that lifted a card must not end in a click: the browser hit-tests
   * the tap again on release and would close the menu the hold just opened.
   */
  function touchend(event: TouchEvent) {
    if (swallowClick || press?.lifted || drag) event.preventDefault();
  }

  function contextmenu(event: MouseEvent) {
    if (press?.touch || drag) event.preventDefault();
  }

  function escape(event: KeyboardEvent) {
    if (event.key === "Escape" && drag) {
      event.preventDefault();
      event.stopPropagation();
      void finish(false);
    }
  }

  const listeners = [
    ["pointerdown", down], ["pointermove", move], ["pointerup", up], ["pointercancel", up],
    ["contextmenu", contextmenu],
  ] as const;
  for (const [name, handler] of listeners) grid.addEventListener(name, handler as EventListener);
  grid.addEventListener("click", click, true);
  grid.addEventListener("touchmove", touchmove, { passive: false });
  grid.addEventListener("touchend", touchend);
  window.addEventListener("keydown", escape, true);
  window.addEventListener("keydown", key);
  return {
    update(next) {
      options = next;
      if (!next.movable && drag) void finish(false);
    },
    destroy() {
      endPress();
      if (drag) cancelAnimationFrame(drag.frame);
      for (const [name, handler] of listeners) grid.removeEventListener(name, handler as EventListener);
      grid.removeEventListener("click", click, true);
      grid.removeEventListener("touchmove", touchmove);
      grid.removeEventListener("touchend", touchend);
      window.removeEventListener("keydown", escape, true);
      window.removeEventListener("keydown", key);
    },
  };
};
