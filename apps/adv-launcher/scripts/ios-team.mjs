// Tauri writes APPLE_DEVELOPMENT_TEAM into the app target only and runs
// xcodebuild without the variable, so the JIT extension would sign with no
// team. Fill the extension's placeholder before Xcode plans signing; restore
// project.pbxproj after the build, as for Tauri's own edit.
import { readFileSync, writeFileSync } from "node:fs";

const team = process.env.APPLE_DEVELOPMENT_TEAM ?? "";
const project = new URL(
  "../src-tauri/gen/apple/adv-launcher.xcodeproj/project.pbxproj",
  import.meta.url,
);
if (/^[A-Z0-9]+$/.test(team)) {
  const text = readFileSync(project, "utf8");
  writeFileSync(project, text.replaceAll('"$(APPLE_DEVELOPMENT_TEAM)"', `"${team}"`));
}
