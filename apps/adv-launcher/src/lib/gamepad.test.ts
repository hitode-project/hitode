import assert from "node:assert/strict";
import test from "node:test";
import { KEY_CATALOG } from "./key-layouts.ts";
import {
  defaultGamepadConfig,
  dpadPresetBindings,
  effectiveButtonSlot,
  findKeyCategory,
  glyphsForDevice,
  isBuiltinModified,
  normalizeGamepadConfig,
  resolveEffectiveProfileId,
  type GamepadDevice,
} from "./gamepad.ts";

test("profile precedence resolves per-game override, then device default, then global", () => {
  const cfg = defaultGamepadConfig();
  cfg.devices.push({
    id: "dev-8bitdo",
    name: "8BitDo Micro",
    detectedStyle: "xbox",
    glyphOverride: "switch",
    swapConfirmCancel: true,
    defaultProfileId: "mouseOnly",
  });
  cfg.gameProfiles.g1 = "action";

  assert.equal(resolveEffectiveProfileId(cfg, "dev-8bitdo", "g1"), "action");
  assert.equal(resolveEffectiveProfileId(cfg, "dev-8bitdo", "g2"), "mouseOnly");
  assert.equal(resolveEffectiveProfileId(cfg, "unknown", "g2"), "adv");
});

test("confirm/cancel swap and glyph override resolve independently per device", () => {
  const dev: GamepadDevice = {
    id: "dev-8bitdo",
    name: "8BitDo Micro",
    detectedStyle: "xbox",
    glyphOverride: "switch",
    swapConfirmCancel: true,
    defaultProfileId: null,
  };
  assert.equal(glyphsForDevice(dev).east.glyph, "A");
  assert.equal(glyphsForDevice(dev).south.glyph, "B");
  assert.equal(effectiveButtonSlot("south", true), "east");
  assert.equal(effectiveButtonSlot("east", true), "south");
  assert.equal(effectiveButtonSlot("west", true), "west");
});

test("isBuiltinModified detects binding and stick changes", () => {
  const cfg = defaultGamepadConfig();
  const adv = cfg.profiles.find((p) => p.id === "adv")!;
  assert.equal(isBuiltinModified(adv), false);

  adv.bindings = { ...adv.bindings, ...dpadPresetBindings("wasd") };
  assert.equal(isBuiltinModified(adv), true);
});

test("findKeyCategory locates keys across all six KEY_CATALOG tabs", () => {
  assert.equal(findKeyCategory({ kind: "vk", vk: 0x0d, label: "Enter" }, KEY_CATALOG), "common");
  assert.equal(findKeyCategory({ kind: "vk", vk: 0x41, label: "A" }, KEY_CATALOG), "alpha");
  assert.equal(findKeyCategory({ kind: "vk", vk: 0x1dc, label: "JIS ¥" }, KEY_CATALOG), "symbol");
  assert.equal(findKeyCategory({ kind: "vk", vk: 0x7c, label: "F13" }, KEY_CATALOG), "fn");
  assert.equal(findKeyCategory({ kind: "vk", vk: 0x67, label: "Num 7" }, KEY_CATALOG), "numpad");
  assert.equal(findKeyCategory({ kind: "mouse", button: "left", label: "左键" }, KEY_CATALOG), "system");
});

test("normalizeGamepadConfig clamps cursor speed and drops dangling profile references", () => {
  const cfg = defaultGamepadConfig();
  cfg.profiles[0]!.cursorSpeed = 9.9;
  cfg.gameProfiles.g1 = "deleted-profile";
  cfg.devices.push({
    id: "d1",
    name: "Pad",
    detectedStyle: "ps",
    glyphOverride: "auto",
    swapConfirmCancel: false,
    defaultProfileId: "deleted-profile",
  });
  const normalized = normalizeGamepadConfig(cfg);
  assert.equal(normalized.profiles[0]!.cursorSpeed, 2.0);
  assert.equal(normalized.gameProfiles.g1, undefined);
  assert.equal(normalized.devices[0]!.defaultProfileId, null);
});
