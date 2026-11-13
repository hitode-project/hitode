import assert from "node:assert/strict";
import test from "node:test";
import { isAndroidExternalPath, isAndroidPermissionBlocked } from "./android-storage.ts";

test("isAndroidExternalPath matches primary and mounted external volumes only", () => {
  assert.equal(isAndroidExternalPath("/storage/emulated/0/Games/Novel"), true);
  assert.equal(isAndroidExternalPath("/storage/9C33-6BBD/Games/Novel"), true);
  assert.equal(isAndroidExternalPath("/storage/USB_DRIVE"), true);
  assert.equal(isAndroidExternalPath("/storage/self/primary/Games"), false);
  assert.equal(isAndroidExternalPath("/data/user/0/org.hitode.launcher/files/imports/abc"), false);
  assert.equal(isAndroidExternalPath("/games/Novel"), false);
});

test("isAndroidPermissionBlocked distinguishes denied external access from unplugged disks", () => {
  const base = {
    platform: "android",
    checked: true,
    supported: true,
    granted: false,
    path: "/storage/emulated/0/Games/Novel",
    offline: null as "disk" | "missing" | null,
  };
  assert.equal(isAndroidPermissionBlocked(base), true);
  assert.equal(isAndroidPermissionBlocked({ ...base, offline: "missing" }), true);
  assert.equal(isAndroidPermissionBlocked({ ...base, path: "/storage/9C33-6BBD/Games/Novel" }), true);
  assert.equal(isAndroidPermissionBlocked({ ...base, offline: "disk" }), false);
  assert.equal(isAndroidPermissionBlocked({ ...base, granted: true }), false);
  assert.equal(isAndroidPermissionBlocked({ ...base, supported: false }), false);
  assert.equal(isAndroidPermissionBlocked({ ...base, checked: false }), false);
  assert.equal(isAndroidPermissionBlocked({ ...base, platform: "ios" }), false);
  assert.equal(
    isAndroidPermissionBlocked({ ...base, path: "/data/user/0/org.hitode.launcher/files/imports/abc" }),
    false,
  );
});
