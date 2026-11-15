import assert from "node:assert/strict";
import test from "node:test";
import {
  clampOpacity,
  clampScale,
  dpadDirections,
  dpadVkMap,
  itemCenter,
  normalizeStoredKeyLayouts,
  pointToNormalized,
  safeBounds,
} from "./key-layouts.ts";
import type { GameLayout } from "./screen.svelte.ts";

test("custom key safe bounds clear cutouts, rounded corners and iOS gesture strips", () => {
  const layout: GameLayout = {
    width: 844,
    height: 390,
    game: { x: 162, y: 0, width: 520, height: 390 },
    cutouts: [{ x: 0, y: 120, width: 34, height: 150 }],
    cornerRadius: 44,
    gestures: { left: 20, right: 20, top: 0, bottom: 21 },
  };
  const ios = safeBounds(layout, 844, 390, false);
  assert.ok(ios.x > 34);
  assert.ok(ios.y + ios.height <= 390 - 21 - 8);

  const android = safeBounds(layout, 844, 390, true);
  assert.ok(android.x > 34);
  assert.ok(android.y + android.height > ios.y + ios.height);
});

test("itemCenter and pointToNormalized keep scaled items inside safe bounds", () => {
  const bounds = { x: 40, y: 12, width: 760, height: 360 };
  const bigKey = {
    id: "k1",
    type: "key" as const,
    rx: 0,
    ry: 1,
    scale: 1.6,
    latch: false,
    binding: { kind: "vk" as const, vk: 0x5a, label: "Z" },
  };
  const placed = itemCenter(bigKey, bounds);
  assert.ok(placed.cx - placed.width / 2 >= bounds.x);
  assert.ok(placed.cy + placed.height / 2 <= bounds.y + bounds.height);

  const norm = pointToNormalized(-50, 999, bigKey, bounds);
  assert.ok(norm.rx > 0 && norm.rx < 0.2);
  assert.ok(norm.ry > 0.8 && norm.ry < 1);
});

test("dpadDirections distinguishes 4-way cardinal from 8-way diagonals", () => {
  assert.deepEqual(dpadDirections(3, -2, 10, true), {
    up: false,
    down: false,
    left: false,
    right: false,
  });
  // Diagonal down-right: 8-way presses both, 4-way picks dominant axis.
  assert.deepEqual(dpadDirections(28, 24, 10, true), {
    up: false,
    down: true,
    left: false,
    right: true,
  });
  assert.deepEqual(dpadDirections(28, 24, 10, false), {
    up: false,
    down: false,
    left: false,
    right: true,
  });
  assert.deepEqual(dpadVkMap("wasd"), { up: 0x57, down: 0x53, left: 0x41, right: 0x44 });
});

test("scale snapping and stored layout normalization preserve built-in presets", () => {
  assert.equal(clampScale(1.015, true), 1);
  assert.equal(clampScale(1.25, true), 1.25);
  assert.equal(clampScale(9, false), 1.6);
  assert.equal(clampOpacity(0.02), 0.15);

  const normalized = normalizeStoredKeyLayouts({
    profiles: [
      {
        id: "user-1",
        name: "My ARPG",
        opacity: 0.7,
        autoDim: false,
        items: [
          {
            id: "a",
            type: "key",
            rx: 0.5,
            ry: 0.5,
            scale: 1.2,
            latch: true,
            binding: { kind: "sys", sys: "menu", label: "菜单" },
          },
        ],
      },
    ],
    games: { g1: "user-1", g2: "missing" },
  });
  assert.equal(normalized.profiles.length, 3);
  assert.equal(normalized.games.g1, "user-1");
  assert.equal(normalized.games.g2, undefined);
  const userItem = normalized.profiles.find((p) => p.id === "user-1")?.items[0];
  assert.equal(userItem?.type, "key");
  if (userItem?.type === "key") {
    // System actions cannot latch even if malformed JSON requested it.
    assert.equal(userItem.latch, false);
  }
});
