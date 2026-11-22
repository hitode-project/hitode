import assert from "node:assert/strict";
import test from "node:test";
import { tap } from "./tap.ts";
import { arrange, drawerEdgeInset, preferredDrawerSide } from "./touch-layout.ts";
import type { GameLayout } from "./screen.svelte.ts";

test("iOS controls stay outside system gesture strips", () => {
  const layout: GameLayout = {
    width: 900, height: 400, game: { x: 130, y: 0, width: 640, height: 400 },
    cutouts: [], cornerRadius: 0, gestures: { left: 30, right: 42, top: 0, bottom: 24 },
  };
  const placement = arrange(layout);
  assert.equal(placement.kind, "side");
  if (placement.kind !== "side") return;
  assert.equal(placement.left.x, 30);
  assert.equal(placement.right.x + placement.right.width, 858);
  assert.equal(placement.left.y + placement.left.height, 376);
});

test("Android controls keep both black bars despite system gesture strips", () => {
  const layout: GameLayout = {
    width: 740, height: 480, game: { x: 50, y: 0, width: 640, height: 480 },
    cutouts: [], cornerRadius: 0, gestures: { left: 24, right: 30, top: 0, bottom: 24 },
  };
  const placement = arrange(layout, true);
  assert.equal(placement.kind, "side");
  assert.deepEqual(placement.left, { x: 6, y: 6, width: 40, height: 468 });
  assert.deepEqual(placement.right, { x: 694, y: 6, width: 40, height: 468 });
});

test("each side keeps its controls when its black bar still fits", () => {
  const layout: GameLayout = {
    width: 740, height: 480, game: { x: 50, y: 0, width: 640, height: 480 },
    cutouts: [], cornerRadius: 0, gestures: { left: 8, right: 24, top: 0, bottom: 24 },
  };

  const left = arrange(layout);
  assert.equal(left.kind, "float");
  assert.deepEqual(left.left, { x: 8, y: 6, width: 38, height: 450 });
  assert.equal(left.right, null);

  const right = arrange({ ...layout, gestures: { ...layout.gestures, left: 24, right: 8 } });
  assert.equal(right.kind, "float");
  assert.equal(right.left, null);
  assert.deepEqual(right.right, { x: 694, y: 6, width: 38, height: 450 });
});

test("an edge button commits only an uncancelled tap", () => {
  let actions = 0;
  const button = tap(() => actions++);
  const touch = (x: number) => ({
    pointerId: 1, pointerType: "touch", isPrimary: true, clientX: x, clientY: 20,
  }) as PointerEvent;
  const click = { detail: 1 } as MouseEvent;
  button.onpointerdown(touch(3));
  assert.equal(actions, 0);
  button.onpointerup(touch(6));
  button.onclick(click);
  assert.equal(actions, 1);

  button.onpointerdown(touch(3));
  button.onpointermove(touch(30));
  button.onpointerup(touch(3));
  button.onclick(click);
  assert.equal(actions, 1);

  button.onpointerdown(touch(3));
  button.onpointercancel(touch(4));
  button.onclick(click);
  assert.equal(actions, 1);
  button.onclick({ detail: 0 } as MouseEvent);
  assert.equal(actions, 2);
});

test("preferredDrawerSide avoids a right-edge camera and computes cutout clearance", () => {
  const base: GameLayout = {
    width: 844, height: 390, game: { x: 112, y: 0, width: 620, height: 390 },
    cutouts: [], cornerRadius: 44, gestures: { left: 0, right: 0, top: 0, bottom: 21 },
  };
  assert.equal(preferredDrawerSide(null), "right");
  assert.equal(preferredDrawerSide(base), "right");
  assert.equal(drawerEdgeInset(base, "left"), 0);
  assert.equal(drawerEdgeInset(base, "right"), 0);

  // Dynamic Island on the right edge (clockwise landscape) -> default drawer to left.
  const rightCam: GameLayout = {
    ...base,
    cutouts: [{ x: 785, y: 132, width: 48, height: 126 }],
  };
  assert.equal(preferredDrawerSide(rightCam), "left");
  assert.equal(drawerEdgeInset(rightCam, "left"), 0);
  assert.equal(drawerEdgeInset(rightCam, "right"), 59);

  // Dynamic Island on the left edge (counter-clockwise landscape) -> default drawer to right.
  const leftCam: GameLayout = {
    ...base,
    cutouts: [{ x: 11, y: 132, width: 48, height: 126 }],
  };
  assert.equal(preferredDrawerSide(leftCam), "right");
  assert.equal(drawerEdgeInset(leftCam, "left"), 59);
  assert.equal(drawerEdgeInset(leftCam, "right"), 0);
});
