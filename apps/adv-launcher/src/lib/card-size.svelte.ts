import { cardFor, columnChoices, columnsFor } from "./arrange";
import { PinchLayout } from "./pinch-layout";
import { prefs, savePrefs } from "./prefs.svelte";
import { resize } from "./reflow";

type Point = { x: number; y: number };

/**
 * Cover size of one library grid. The grid fills its width with whole
 * columns of at least `prefs[key]` px; the slider, ⌘± and the 排列 sheet step
 * the column count. A pinch shifts through the counts like gears
 * (pinch-layout.ts) and keeps the one it rests on.
 */
export class CardSize {
  #key: "cardWide" | "cardPhone";
  #gap: number;
  #min: number;
  #max: number;
  /** The pinch in progress, until it rests. */
  #pinch: PinchLayout | null = null;

  /** Content width of the grid, bound by the page. */
  width = $state(0);
  grid: HTMLElement | undefined;
  scroller: HTMLElement | undefined;

  /** Column counts that fit, fewest first. */
  get choices(): number[] {
    return columnChoices(this.width, this.#gap, this.#min, this.#max);
  }

  get columns(): number {
    return columnsFor(this.width, prefs[this.#key], this.#gap);
  }

  /** The grid's CSS columns track list. */
  get template(): string {
    return `repeat(auto-fill, minmax(min(${prefs[this.#key]}px, 100%), 1fr))`;
  }

  constructor(key: "cardWide" | "cardPhone", gap: number, min: number, max: number) {
    this.#key = key;
    this.#gap = gap;
    this.#min = min;
    this.#max = max;
  }

  /** Shows `columns` (kept within the choices), animating from the current layout. */
  set(columns: number, at?: Point): void {
    const { grid, scroller, choices } = this;
    const target = Math.min(Math.max(columns, choices[0] ?? columns), choices.at(-1) ?? columns);
    if (!grid || !scroller || this.#pinch || target === this.columns) return;
    void resize(grid, scroller, () => this.#apply(target), at);
  }

  pinch(scale: number, at: Point): void {
    const { grid, scroller } = this;
    if (!grid || !scroller || !grid.children.length) return;
    this.#pinch ??= new PinchLayout(grid, scroller, this.columns, this.choices, (columns, point, clear) => {
      this.#pinch = null;
      // The cards already show `columns`: laying it out moves nothing.
      void resize(grid, scroller, () => {
        clear();
        this.#apply(columns);
      }, point);
    });
    this.#pinch.pull(scale, at);
  }

  pinchEnd(): void {
    this.#pinch?.release();
  }

  #apply(columns: number) {
    if (columns === this.columns) return;
    prefs[this.#key] = cardFor(columns, this.width, this.#gap);
    savePrefs();
  }
}
