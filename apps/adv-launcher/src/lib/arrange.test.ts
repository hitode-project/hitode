import assert from "node:assert/strict";
import test from "node:test";
import { cardFor, columnChoices, columnsFor, moveTo, placeAmong } from "./arrange.ts";

test("a move in a filtered view keeps hidden games in place", () => {
  const order = ["a", "x", "b", "y", "c"];
  // c dragged before a among the visible a, b, c.
  assert.deepEqual(placeAmong(order, moveTo(["a", "b", "c"], "c", 0), "c"), ["c", "a", "x", "b", "y"]);
  // a dragged to the end lands after the last visible game.
  assert.deepEqual(placeAmong(order, moveTo(["a", "b", "c"], "a", 2), "a"), ["x", "b", "y", "c", "a"]);
  assert.deepEqual(placeAmong(["a"], ["a"], "a"), ["a"]);
});

test("a chosen column count fills the width and survives the grid's own rounding", () => {
  for (const width of [320, 777, 1180]) {
    for (const columns of [2, 3, 5, 8]) {
      assert.equal(columnsFor(width, cardFor(columns, width, 24), 24), columns);
    }
  }
  assert.deepEqual(columnChoices(600, 12, 80, 200), [3, 4, 5, 6]);
});
