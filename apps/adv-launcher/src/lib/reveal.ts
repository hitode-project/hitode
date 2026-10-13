import { inTauri } from "./bridge";
import { device } from "./device.svelte";
import { revealPath } from "./host";

/** "在访达中显示" is offered in the macOS desktop app only. */
export function canReveal(): boolean {
  return inTauri && device.platform === "macos" && !device.phone;
}

/** Shows `path` in Finder. */
export function reveal(path: string): void {
  revealPath(path).catch((error: unknown) => console.error("reveal_path failed", error));
}
