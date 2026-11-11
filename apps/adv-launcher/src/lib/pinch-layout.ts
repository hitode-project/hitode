import { hapticTick } from "./host";
import { REFLOW, reducedMotion } from "./reflow";

type Point = { x: number; y: number };

/** The column-count spring: settles in about a quarter second, overshooting a few percent. */
const OMEGA = 22;
const ZETA = 0.72;
/** Within a count the covers follow this share of the pinch… */
const STICK = 0.25;
/** …and never drift more than this many columns from it, past the last count too. */
const REACH = 0.35;
/** How far past halfway the fingers go before the next count takes over. */
const HYSTERESIS = 0.08;
/** Cards this far off screen still move, so none pops in at an edge. */
const MARGIN = 120;

/**
 * A pinch over the library grid played with transforms alone. The column
 * count is a spring the fingers pull, held near whole counts and clicking
 * from one to the next with a haptic tick; every card sits where it would be
 * at that fractional count, between its slots in the two counts around it.
 * The page lays nothing out until the spring rests on a whole count, which
 * `onrest` then applies with the cards already in place.
 *
 * Cards scale as one with their top-left corner fixed; the grid has class
 * `pinching` meanwhile, so the page can fade the titles, which would
 * overlap. Cards below the first are placed by the column width, the cover
 * ratio and the caption height measured at the start, so all rows match.
 */
export class PinchLayout {
  #grid: HTMLElement;
  #scroller: HTMLElement;
  #cards: HTMLElement[];
  #lo: number;
  #hi: number;
  #onrest: (columns: number, at: Point, clear: () => void) => void;

  /** Layout measured at the start: count, column width, gaps, cover ratio, caption height. */
  #n0: number;
  #w0: number;
  #width: number;
  #gap: number;
  #rowGap: number;
  #ratio: number;
  #caption: number;
  /** Screen corner of the first card, the origin of every slot. */
  #left: number;
  #top: number;
  /** How far the list could scroll at the start (below zero when it was short), and the grid's height then. */
  #room: number;
  #tall: number;

  /** The spring: fractional column count and its speed per second. */
  #c: number;
  #v = 0;
  #target: number;
  /** The whole count the fingers chose. */
  #gear: number;
  /** Vertical shift that keeps the point under the fingers in place. */
  #shift = 0;
  #held = false;
  /** Per gesture: the column width when it began, and the point it pinches about. */
  #from = 0;
  #at: Point = { x: 0, y: 0 };
  #anchor = { index: 0, along: 0 };
  #frame = 0;
  #then = 0;
  #touched = new Set<HTMLElement>();

  constructor(
    grid: HTMLElement, scroller: HTMLElement, columns: number, choices: number[],
    onrest: (columns: number, at: Point, clear: () => void) => void,
  ) {
    this.#grid = grid;
    this.#scroller = scroller;
    this.#cards = [...grid.children] as HTMLElement[];
    this.#lo = choices[0] ?? columns;
    this.#hi = choices.at(-1) ?? columns;
    this.#onrest = onrest;
    for (const animation of grid.getAnimations({ subtree: true })) if (animation.id === REFLOW) animation.cancel();
    const style = getComputedStyle(grid);
    const first = this.#cards[0]!;
    const card = first.getBoundingClientRect();
    const cover = (first.firstElementChild as HTMLElement).getBoundingClientRect();
    this.#n0 = columns;
    this.#w0 = card.width;
    this.#gap = parseFloat(style.columnGap) || 0;
    this.#rowGap = parseFloat(style.rowGap) || 0;
    this.#width = columns * card.width + (columns - 1) * this.#gap;
    this.#ratio = cover.height / card.width;
    const below = this.#cards[columns];
    this.#caption = (below ? below.getBoundingClientRect().top - card.top - this.#rowGap : card.height) - cover.height;
    this.#left = card.left;
    this.#top = card.top;
    const view = scroller.getBoundingClientRect();
    const foot = grid.getBoundingClientRect().bottom - view.top + scroller.scrollTop + parseFloat(getComputedStyle(scroller).paddingBottom);
    this.#room = foot - scroller.clientHeight;
    this.#c = this.#target = this.#gear = columns;
    this.#tall = this.#bottom(columns);
    grid.classList.add("pinching");
  }

  /** The fingers pinched to `scale` since this gesture began, about `at`. */
  pull(scale: number, at: Point): void {
    if (!this.#held) this.#grab(at);
    const width = this.#from * scale;
    const wanted = (this.#width + this.#gap) / (width + this.#gap);
    if (Math.abs(wanted - this.#gear) > 0.5 + HYSTERESIS) {
      const gear = Math.min(Math.max(Math.round(wanted), this.#lo), this.#hi);
      if (gear !== this.#gear) {
        this.#gear = gear;
        hapticTick();
      }
    }
    this.#target = this.#gear + REACH * Math.tanh((STICK * (wanted - this.#gear)) / REACH);
    this.#run();
  }

  /** The fingers lifted: the spring settles on the chosen count, then it is applied. */
  release(): void {
    this.#held = false;
    this.#target = this.#gear;
    this.#run();
  }

  /** A new gesture, maybe while the last one still settles: it starts from what is on screen. */
  #grab(at: Point) {
    this.#held = true;
    this.#from = this.#column(this.#c);
    this.#at = at;
    // The card nearest the fingers, and how far down it they are, stays under them.
    const index = this.#nearest(at);
    const top = this.#top + this.#place(index, this.#c).y + this.#shift;
    this.#anchor = { index, along: (at.y - top) / this.#height(this.#column(this.#c)) };
  }

  #run() {
    if (this.#frame) return;
    this.#then = performance.now();
    this.#frame = requestAnimationFrame(this.#tick);
  }

  #tick = (now: number) => {
    this.#frame = 0;
    const dt = Math.min((now - this.#then) / 1000, 1 / 30);
    this.#then = now;
    if (reducedMotion()) {
      [this.#c, this.#v] = [this.#target, 0];
    } else {
      this.#v += (OMEGA * OMEGA * (this.#target - this.#c) - 2 * ZETA * OMEGA * this.#v) * dt;
      this.#c += this.#v * dt;
    }
    const resting = !this.#held && Math.abs(this.#c - this.#target) < 0.002 && Math.abs(this.#v) < 0.02;
    if (resting) this.#c = this.#target;
    this.#render();
    if (resting) {
      this.#onrest(this.#gear, this.#at, () => this.#clear());
    } else {
      this.#run();
    }
  };

  #render() {
    const c = this.#c;
    const anchor = this.#place(this.#anchor.index, c);
    const keep = this.#at.y - this.#top - anchor.y - this.#anchor.along * this.#height(anchor.w);
    // Shown as the list could scroll at this count: the grid's top never comes
    // below the top of the list, nor its foot above the bottom, so laying the
    // count out at the end needs no scroll the list cannot make.
    const scroll = this.#scroller.scrollTop;
    const room = Math.max(0, this.#room - (this.#tall - this.#bottom(c)));
    this.#shift = Math.min(Math.max(keep, scroll - room), scroll);
    const view = this.#scroller.getBoundingClientRect();
    // The cards on screen now or at the start, and those between.
    const first = Math.min(this.#firstBelow(view.top - MARGIN, c), this.#firstBelow(view.top - MARGIN, this.#n0, 0));
    const last = Math.max(this.#lastAbove(view.bottom + MARGIN, c), this.#lastAbove(view.bottom + MARGIN, this.#n0, 0));
    for (let i = first; i <= last; i++) {
      const card = this.#cards[i]!;
      const from = this.#slot(i, this.#n0);
      const to = this.#place(i, c);
      if (!this.#touched.has(card)) {
        this.#touched.add(card);
        card.style.transformOrigin = "0 0";
        card.style.willChange = "transform";
      }
      card.style.transform = `translate(${to.x - from.x}px, ${to.y - from.y + this.#shift}px) scale(${to.w / this.#w0})`;
    }
  }

  #clear() {
    for (const card of this.#touched) {
      card.style.transform = "";
      card.style.transformOrigin = "";
      card.style.willChange = "";
    }
    this.#grid.classList.remove("pinching");
  }

  /** The grid's height at fractional count `c`: the foot of its last card. */
  #bottom(c: number): number {
    const last = this.#place(this.#cards.length - 1, c);
    return last.y + this.#height(last.w);
  }

  #column(n: number): number {
    return (this.#width - this.#gap * (n - 1)) / n;
  }

  #height(column: number): number {
    return column * this.#ratio + this.#caption;
  }

  /** Card `i` in whole count `n`, relative to the first card. */
  #slot(i: number, n: number) {
    const w = this.#column(n);
    return { x: (i % n) * (w + this.#gap), y: Math.floor(i / n) * (this.#height(w) + this.#rowGap), w };
  }

  /** Card `i` at fractional count `c`: between its slots in the counts around it (beyond them past the ends). */
  #place(i: number, c: number) {
    const n = Math.max(1, Math.floor(c));
    const f = c - n;
    const a = this.#slot(i, n);
    const b = this.#slot(i, n + 1);
    return { x: a.x + (b.x - a.x) * f, y: a.y + (b.y - a.y) * f, w: a.w + (b.w - a.w) * f };
  }

  /** The card nearest a point on screen at the current count. */
  #nearest(at: Point): number {
    const c = this.#c;
    const x = at.x - this.#left;
    const y = at.y - this.#top - this.#shift;
    const reach = this.#height(this.#column(c)) + this.#rowGap;
    let best = 0;
    let distance = Infinity;
    for (let i = this.#firstBelow(at.y - reach, c); i <= this.#lastAbove(at.y + reach, c); i++) {
      const p = this.#place(i, c);
      const d = Math.hypot(Math.max(p.x - x, 0, x - p.x - p.w), Math.max(p.y - y, 0, y - p.y - this.#height(p.w)));
      if (d < distance) [best, distance] = [i, d];
    }
    return best;
  }

  /** The first card whose bottom is below `y` on screen (cards go down with their index). */
  #firstBelow(y: number, c: number, shift = this.#shift): number {
    let [lo, hi] = [0, this.#cards.length];
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const p = this.#place(mid, c);
      if (this.#top + p.y + shift + this.#height(p.w) < y) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  /** The last card whose top is above `y` on screen. */
  #lastAbove(y: number, c: number, shift = this.#shift): number {
    let [lo, hi] = [0, this.#cards.length];
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (this.#top + this.#place(mid, c).y + shift < y) lo = mid + 1;
      else hi = mid;
    }
    return lo - 1;
  }
}
