import assert from "node:assert/strict";
import test from "node:test";
import { componentForDll, exitedWithoutFrame, missingRuntimeComponent } from "./redist.ts";

test("componentForDll matches VC140 MFC modules regardless of case", () => {
  assert.deepEqual(componentForDll("mfc140u.dll"), {
    id: "vc140-mfc",
    name: "Microsoft Visual C++ 2015–2022",
    dll: "mfc140u.dll",
  });
  assert.deepEqual(componentForDll("MFC140.DLL"), {
    id: "vc140-mfc",
    name: "Microsoft Visual C++ 2015–2022",
    dll: "MFC140.DLL",
  });
  assert.equal(componentForDll("vcruntime140.dll"), null);
  assert.equal(componentForDll("d3d11.dll"), null);
});

test("missingRuntimeComponent parses host missing_dll errors", () => {
  assert.deepEqual(missingRuntimeComponent("missing_dll:mfc140u.dll"), {
    id: "vc140-mfc",
    name: "Microsoft Visual C++ 2015–2022",
    dll: "mfc140u.dll",
  });
  assert.equal(missingRuntimeComponent("missing_dll:unknown.dll"), null);
  assert.equal(missingRuntimeComponent("other error"), null);
});

test("exitedWithoutFrame detects silent zero-frame exits only", () => {
  assert.equal(exitedWithoutFrame({ summary: { timeline: { frames: 0 } } }, false, false, false), true);
  assert.equal(exitedWithoutFrame({ summary: { timeline: { frames: 1 } } }, false, false, false), false);
  assert.equal(exitedWithoutFrame({ summary: { timeline: { frames: 0 } } }, true, false, false), false);
  assert.equal(exitedWithoutFrame({ summary: { timeline: { frames: 0 } } }, false, true, false), false);
  assert.equal(exitedWithoutFrame({ summary: { timeline: { frames: 0 } } }, false, false, true), false);
  assert.equal(
    exitedWithoutFrame({ error: "boom", summary: { timeline: { frames: 0 } } }, false, false, false),
    false,
  );
});
