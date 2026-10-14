import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Whether the page runs inside the Tauri shell (false in a plain browser during `pnpm dev`). */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/**
 * Controls the host does not back yet show only in the browser preview, so the
 * app never offers a setting that does nothing.
 */
export const preview = !inTauri;

/** Calls a launcher command; outside Tauri it resolves to `fallback` so pages work in a browser. */
export async function call<T>(command: string, args: Record<string, unknown>, fallback: T): Promise<T> {
  if (!inTauri) return fallback;
  return invoke<T>(command, args);
}

export interface HostInfo {
  version: string;
  /** "macos", "windows", "linux", "android", "ios". */
  platform: string;
  /** A native game view is attached under the webview. */
  gameView: boolean;
}

export function hostInfo(): Promise<HostInfo> {
  return call<HostInfo | null>("host_info", {}, null).then((info) => info ?? Promise.reject(new Error("not in Tauri")));
}

/** Subscribes to an event the Rust side emits (e.g. "pause" from the menu); a no-op outside Tauri. */
export function onHostEvent<T = unknown>(name: string, handler: (payload: T) => void): () => void {
  if (!inTauri) return () => {};
  let unlisten: (() => void) | null = null;
  let dropped = false;
  void listen<T>(name, (event) => handler(event.payload)).then((stop) => {
    if (dropped) stop();
    else unlisten = stop;
  });
  return () => {
    dropped = true;
    unlisten?.();
  };
}
