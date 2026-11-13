import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Whether the page runs inside the Tauri shell (false in a plain browser during `pnpm dev`). */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/**
 * Controls the host does not back yet show only in the browser preview, so the
 * app never offers a setting that does nothing.
 */
export const preview = !inTauri;

export interface HostInfo {
  version: string;
  /** "macos", "windows", "linux", "android", "ios". */
  platform: string;
  /** A native game view is attached under the webview. */
  gameView: boolean;
}

const ACL_PATTERNS = ["not allowed by ACL", "not allowed on window", "not allowed on origin", "not allowed. "];
const STARTUP_DELAYS_MS = [16, 32, 64, 128, 250, 500, 1000];

/** Whether `error` is a Tauri IPC capability / origin rejection (release or debug). */
export function isAclError(error: unknown): boolean {
  const text = String(error);
  return ACL_PATTERNS.some((pattern) => text.includes(pattern));
}

export interface BridgeDeps {
  inTauri: boolean;
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(name: string, handler: (payload: T) => void) => Promise<() => void>;
  wait?: ((ms: number) => Promise<void>) | undefined;
  onError?: ((message: string, error: unknown) => void) | undefined;
}

/**
 * On Android, wry's `RustWebViewClient.currentUrl` starts as `"about:blank"`
 * until UI-thread `onPageStarted` runs. IPC calls sent during initial module
 * evaluation can arrive first on the JavaBridge thread and fail Tauri's
 * local-origin ACL check. Retrying ACL rejections until the first IPC call or
 * listener succeeds covers that startup race without delaying real permission
 * errors later in the session.
 */
export function createHostBridge(deps: BridgeDeps) {
  const wait = deps.wait ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const onError = deps.onError ?? ((message: string, error: unknown) => console.error(message, error));
  let ready = false;
  let cachedInfo: HostInfo | null = null;
  let pendingInfo: Promise<HostInfo> | null = null;

  async function runWithStartupRetry<T>(operation: () => Promise<T>, cancelled?: () => boolean): Promise<T> {
    for (let attempt = 0; ; attempt++) {
      try {
        const result = await operation();
        ready = true;
        return result;
      } catch (error) {
        const delay = !ready && !cancelled?.() && isAclError(error) ? STARTUP_DELAYS_MS[attempt] : undefined;
        if (delay === undefined) throw error;
        await wait(delay);
        if (cancelled?.()) throw error;
      }
    }
  }

  function invokeHost<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    if (!deps.inTauri) return Promise.reject(new Error("not in Tauri"));
    return runWithStartupRetry(() => (args === undefined ? deps.invoke<T>(command) : deps.invoke<T>(command, args)));
  }

  async function call<T>(command: string, args: Record<string, unknown>, fallback: T): Promise<T> {
    if (!deps.inTauri) return fallback;
    return invokeHost<T>(command, args);
  }

  function hostInfo(): Promise<HostInfo> {
    if (!deps.inTauri) return Promise.reject(new Error("not in Tauri"));
    if (cachedInfo) return Promise.resolve(cachedInfo);
    pendingInfo ??= invokeHost<HostInfo>("host_info").then(
      (info) => {
        cachedInfo = info;
        pendingInfo = null;
        return info;
      },
      (error) => {
        pendingInfo = null;
        throw error;
      },
    );
    return pendingInfo;
  }

  function onHostEvent<T = unknown>(name: string, handler: (payload: T) => void): () => void {
    if (!deps.inTauri) return () => {};
    let unlisten: (() => void) | null = null;
    let dropped = false;
    void runWithStartupRetry(
      () =>
        deps.listen<T>(name, (payload) => {
          if (!dropped) handler(payload);
        }),
      () => dropped,
    ).then(
      (stop) => {
        if (dropped) stop();
        else unlisten = stop;
      },
      (error) => {
        if (!dropped) onError(`listen(${name}) failed`, error);
      },
    );
    return () => {
      dropped = true;
      unlisten?.();
    };
  }

  return { invokeHost, call, hostInfo, onHostEvent };
}

const bridge = createHostBridge({
  inTauri,
  invoke: <T>(command: string, args?: Record<string, unknown>) =>
    args === undefined ? invoke<T>(command) : invoke<T>(command, args),
  listen: <T>(name: string, handler: (payload: T) => void) => listen<T>(name, (event) => handler(event.payload)),
});

/** Invokes a Tauri command or plugin command, retrying startup local-origin ACL races. */
export function invokeHost<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return bridge.invokeHost<T>(command, args);
}

/** Calls a launcher command; outside Tauri it resolves to `fallback` so pages work in a browser. */
export function call<T>(command: string, args: Record<string, unknown>, fallback: T): Promise<T> {
  return bridge.call<T>(command, args, fallback);
}

export function hostInfo(): Promise<HostInfo> {
  return bridge.hostInfo();
}

/** Subscribes to an event the Rust side emits (e.g. "pause" from the menu); a no-op outside Tauri. */
export function onHostEvent<T = unknown>(name: string, handler: (payload: T) => void): () => void {
  return bridge.onHostEvent<T>(name, handler);
}
