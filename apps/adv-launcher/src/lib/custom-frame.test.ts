import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  contentArea,
  customRect,
  formatAspect,
  formatZoom,
  rectToCustom,
} from "./custom-frame.ts";
import { arrange } from "./touch-layout.ts";
import type { GameLayout } from "./screen.svelte.ts";

describe("custom frame geometry", () => {
  it("computes cutout-cleared content area symmetrically", () => {
    const layout: GameLayout = {
      width: 844,
      height: 390,
      game: { x: 0, y: 0, width: 844, height: 390 },
      cutouts: [{ x: 810, y: 90, width: 34, height: 210 }],
      cornerRadius: 47,
      gestures: { top: 0, right: 0, bottom: 21, left: 0 },
    };
    assert.deepEqual(contentArea(layout), { x: 34, y: 0, width: 776, height: 390 });
  });

  it("round-trips arbitrary non-uniform rectangles through aspect and zoom", () => {
    const area = { x: 34, y: 0, width: 776, height: 390 };
    for (const [w, h] of [
      [776, 390],
      [620, 390],
      [520, 390],
      [600, 312],
      [420, 280],
    ]) {
      const { aspect, zoom } = rectToCustom(area, w, h);
      const rect = customRect(area, aspect, zoom);
      assert.ok(Math.abs(rect.width - w) <= 2, `width ${rect.width} vs ${w}`);
      assert.ok(Math.abs(rect.height - h) <= 2, `height ${rect.height} vs ${h}`);
      assert.ok(rect.x >= area.x && rect.x + rect.width <= area.x + area.width);
      assert.ok(rect.y >= area.y && rect.y + rect.height <= area.y + area.height);
    }
  });

  it("shrinking a widescreen frame lets default touch controls move into side margins", () => {
    const base: GameLayout = {
      width: 844,
      height: 390,
      game: { x: 75, y: 0, width: 694, height: 390 },
      cutouts: [{ x: 0, y: 132, width: 49, height: 126 }],
      cornerRadius: 47,
      gestures: { top: 0, right: 0, bottom: 21, left: 0 },
    };
    assert.equal(arrange(base).kind, "float");
    const area = contentArea(base);
    const shrunk = customRect(area, 1600, 850);
    assert.equal(arrange({ ...base, game: shrunk }).kind, "side");
  });

  it("formats standard and arbitrary aspect labels", () => {
    assert.equal(formatAspect(1333), "4:3");
    assert.equal(formatAspect(1600), "16:10");
    assert.equal(formatAspect(1778), "16:9");
    assert.equal(formatAspect(1850), "1.85:1");
    assert.equal(formatZoom(850), "85%");
  });
});
