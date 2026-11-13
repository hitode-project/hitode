import { isAndroidExternalPath } from "./android-storage";
import { inTauri } from "./bridge";
import { device } from "./device.svelte";
import { revealDrive, revealPath } from "./host";

export { isAndroidExternalPath };

/**
 * Whether the system file manager can show `path`: anything in the macOS
 * desktop app; on Android, folders on external-storage volumes, not private copies.
 */
export function canReveal(path = ""): boolean {
  if (!inTauri) return false;
  if (device.platform === "macos") return !device.phone;
  return device.platform === "android" && isAndroidExternalPath(path);
}

/** Shows drive C:, where games keep data outside their folders. */
export function revealC(): void {
  revealDrive().catch((error: unknown) => console.error("reveal_drive failed", error));
}

/** Shows `path` in Finder, or in the Android Files app. */
export function reveal(path: string): void {
  revealPath(path).catch((error: unknown) => console.error("reveal_path failed", error));
}
