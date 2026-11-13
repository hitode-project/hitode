import assert from "node:assert/strict";
import test from "node:test";
import { isAndroidExternalPath } from "./android-storage.ts";

// spec: AND-APP-9
test("isAndroidExternalPath accepts primary and removable volumes while rejecting private or synthetic roots", () => {
  assert.equal(isAndroidExternalPath("/storage/emulated/0"), true);
  assert.equal(isAndroidExternalPath("/storage/emulated/0/Games/Novel"), true);
  assert.equal(isAndroidExternalPath("/storage/emulated/10/Games/Novel"), true);
  assert.equal(isAndroidExternalPath("/storage/9C33-6BBD"), true);
  assert.equal(isAndroidExternalPath("/storage/9C33-6BBD/Games/Novel"), true);

  assert.equal(isAndroidExternalPath(""), false);
  assert.equal(isAndroidExternalPath("/storage"), false);
  assert.equal(isAndroidExternalPath("/storage/"), false);
  assert.equal(isAndroidExternalPath("/storage/emulated"), false);
  assert.equal(isAndroidExternalPath("/storage/emulated/"), false);
  assert.equal(isAndroidExternalPath("/storage/emulated/bad/Games"), false);
  assert.equal(isAndroidExternalPath("/storage/self/primary/Games"), false);
  assert.equal(isAndroidExternalPath("/data/user/0/org.hitode.launcher/files/imports/abc"), false);
});
