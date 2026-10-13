import { invoke } from "@tauri-apps/api/core";

/** Whether the page runs inside the Tauri shell (false in a plain browser during `pnpm dev`). */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Calls a launcher command; outside Tauri it resolves to `fallback` so pages work in a browser. */
export async function call<T>(command: string, args: Record<string, unknown>, fallback: T): Promise<T> {
  if (!inTauri) return fallback;
  return invoke<T>(command, args);
}

export interface HostInfo {
  /** "macos", "windows", "linux", "android", "ios". */
  platform: string;
  /** A native game view is attached under the webview. */
  gameView: boolean;
}

export function hostInfo(): Promise<HostInfo> {
  const platform = /Mac/.test(navigator.userAgent) ? "macos" : /Windows/.test(navigator.userAgent) ? "windows" : "linux";
  return call("host_info", {}, { platform, gameView: false });
}
