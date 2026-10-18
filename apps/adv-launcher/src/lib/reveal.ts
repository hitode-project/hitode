import { inTauri } from "./bridge";
import { device } from "./device.svelte";
import { revealPath } from "./host";

/**
 * Whether the system file manager can show `path`: anything in the macOS
 * desktop app; on Android, folders in shared storage, not private copies.
 */
export function canReveal(path = ""): boolean {
  if (!inTauri) return false;
  if (device.platform === "macos") return !device.phone;
  return device.platform === "android" && /^\/storage\/emulated\/0(\/|$)/.test(path);
}

/** Shows `path` in Finder, or in the Android Files app. */
export function reveal(path: string): void {
  revealPath(path).catch((error: unknown) => console.error("reveal_path failed", error));
}
