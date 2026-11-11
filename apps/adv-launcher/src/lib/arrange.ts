import type { Game } from "./types";

// Library order and grid density as pure functions; the library screens own
// the pointer handling (card-drag.ts) and the animation (reflow.ts).

/** "custom" is the order of the host library (HOST-LIB-10); the others sort it. */
export type LibrarySort = "custom" | "recent" | "title" | "brand" | "year";

/** The games in display order. Ties keep the custom order. */
export function sortGames(games: readonly Game[], sort: LibrarySort): Game[] {
  const list = [...games];
  const collator = new Intl.Collator("ja");
  if (sort === "recent") list.sort((a, b) => (b.lastPlayed ?? -1) - (a.lastPlayed ?? -1));
  if (sort === "title") list.sort((a, b) => collator.compare(a.title, b.title));
  if (sort === "brand") list.sort((a, b) => collator.compare(a.brand, b.brand) || collator.compare(a.title, b.title));
  if (sort === "year") list.sort((a, b) => b.year - a.year);
  return list;
}

/** `ids` with `id` moved to `index`. */
export function moveTo(ids: readonly string[], id: string, index: number): string[] {
  const rest = ids.filter((other) => other !== id);
  rest.splice(Math.max(0, Math.min(index, rest.length)), 0, id);
  return rest;
}

/**
 * The full order after `id` moved within `visible`, a filtered part of
 * `order`: it lands before the game that now follows it on screen, or after
 * the one before it when it ends the list. Hidden games keep their places.
 */
export function placeAmong(order: readonly string[], visible: readonly string[], id: string): string[] {
  const at = visible.indexOf(id);
  const rest = order.filter((other) => other !== id);
  const next = visible[at + 1];
  const prev = visible[at - 1];
  const index = next !== undefined ? rest.indexOf(next) : prev !== undefined ? rest.indexOf(prev) + 1 : rest.length;
  rest.splice(index, 0, id);
  return rest;
}

/** Columns of an `auto-fill, minmax(card, 1fr)` grid. */
export function columnsFor(width: number, card: number, gap: number): number {
  return Math.max(1, Math.floor((width + gap) / (card + gap)));
}

/** The card width that fills `width` with exactly `columns`. */
export function cardFor(columns: number, width: number, gap: number): number {
  return Math.floor((width - (columns - 1) * gap) / columns);
}

/** Column counts whose cards stay between `min` and `max` px wide, fewest first. */
export function columnChoices(width: number, gap: number, min: number, max: number): number[] {
  const choices: number[] = [];
  for (let n = 1; cardFor(n, width, gap) >= min; n++) {
    if (cardFor(n, width, gap) <= max) choices.push(n);
  }
  return choices;
}
