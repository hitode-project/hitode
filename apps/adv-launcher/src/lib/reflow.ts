import { tick } from "svelte";

// FLIP for the library grid: measure the cards, change the layout, then play
// each card from where it was. Measurements include running animations, so a
// change in the middle of one continues from what is on screen.

const EASE = "cubic-bezier(0.2, 0.8, 0.2, 1)";
/** The id of the animations played here. */
export const REFLOW = "reflow";

/** Each card's rect on screen and its layout box in the grid. */
export type Snapshot = Map<Element, { rect: DOMRect; x: number; y: number; w: number }>;

export function measure(grid: HTMLElement): Snapshot {
  const snapshot: Snapshot = new Map();
  for (const card of grid.children as unknown as HTMLElement[]) {
    snapshot.set(card, { rect: card.getBoundingClientRect(), x: card.offsetLeft, y: card.offsetTop, w: card.offsetWidth });
  }
  return snapshot;
}

export function reducedMotion(): boolean {
  return matchMedia("(prefers-reduced-motion: reduce)").matches;
}

const reflowing = (card: Element) => card.getAnimations({ subtree: true }).filter((a) => a.id === REFLOW);

/**
 * Animates the cards of `grid` from their rects in `before` to their places
 * now, except `skip` (a card the pointer holds). A card still animating to
 * an unchanged place keeps its animation, so its speed never jumps. A card
 * whose width changed scales its cover (the first child) and moves its
 * captions with the cover's foot, so text never stretches. Cards off screen
 * before and after jump.
 */
export function play(grid: HTMLElement, before: Snapshot, { skip = null as Element | null, duration = 260 } = {}): void {
  const motion = !reducedMotion();
  const view = { top: -200, bottom: innerHeight + 200 };
  const seen = (r: DOMRect) => r.bottom > view.top && r.top < view.bottom;
  // Layout reads first, then cancels, then rect reads: one layout pass.
  const cards = ([...grid.children] as HTMLElement[]).flatMap((card) => {
    const from = before.get(card);
    if (!from || card === skip) return [];
    const placed = from.x === card.offsetLeft && from.y === card.offsetTop && from.w === card.offsetWidth;
    const running = reflowing(card);
    if (placed && running.length) return [];
    return [{ card, from: from.rect, running }];
  });
  for (const { running } of cards) for (const animation of running) animation.cancel();
  if (!motion) return;
  const options = { duration, easing: EASE, id: REFLOW };
  for (const { card, from } of cards) {
    const to = card.getBoundingClientRect();
    if (!seen(from) && !seen(to)) continue;
    const dx = from.left - to.left;
    const dy = from.top - to.top;
    const scale = from.width / to.width;
    const resized = Math.abs(scale - 1) > 0.002;
    if (Math.abs(dx) < 0.5 && Math.abs(dy) < 0.5 && !resized) continue;
    card.animate([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: "none" }], options);
    if (!resized) continue;
    const [cover, ...captions] = card.children as unknown as HTMLElement[];
    if (!cover) continue;
    // A static origin: an animated one would keep the animation off the compositor.
    cover.style.transformOrigin = "0 0";
    cover.animate([{ transform: `scale(${scale})` }, { transform: "none" }], options);
    const foot = (scale - 1) * cover.offsetHeight;
    for (const caption of captions) caption.animate([{ transform: `translateY(${foot}px)` }, { transform: "none" }], options);
  }
}

/** The card nearest the point (default: the top-left of the visible grid), for keeping it in place. */
export function anchorAt(grid: HTMLElement, scroller: HTMLElement, point?: { x: number; y: number }): HTMLElement | null {
  const view = scroller.getBoundingClientRect();
  const x = point?.x ?? view.left;
  const y = point?.y ?? view.top;
  let best: HTMLElement | null = null;
  let distance = Infinity;
  for (const card of grid.children as unknown as HTMLElement[]) {
    const r = card.getBoundingClientRect();
    if (r.bottom < view.top || r.top > view.bottom) continue;
    const d = Math.hypot(Math.max(r.left - x, 0, x - r.right), Math.max(r.top - y, 0, y - r.bottom));
    if (d < distance) [best, distance] = [card, d];
  }
  return best;
}

/**
 * Applies a change of card size (`apply`), keeping the card nearest `point`
 * where it is on screen, and animates the grid into the new layout.
 */
export async function resize(grid: HTMLElement, scroller: HTMLElement, apply: () => void, point?: { x: number; y: number }): Promise<void> {
  const anchor = anchorAt(grid, scroller, point);
  const top = anchor ? anchor.getBoundingClientRect().top : 0;
  const before = measure(grid);
  apply();
  await tick();
  // The anchor's layout top goes where it was seen.
  if (anchor) scroller.scrollTop += anchor.offsetTop + grid.getBoundingClientRect().top - top;
  play(grid, before, { duration: 320 });
}
