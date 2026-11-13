import { isAndroidExternalPath, isAndroidPermissionBlocked } from "./android-storage";
import { inTauri } from "./bridge";
import { device } from "./device.svelte";
import { library } from "./library.svelte";
import type { Game } from "./types";
import { fetchStorageStatus, invokeRequestStorage } from "./storage";

class AndroidStorage {
  granted = $state(true);
  supported = $state(false);
  checked = $state(false);
  busy = $state(false);
  #refreshing: Promise<void> | null = null;

  constructor() {
    if (!inTauri || typeof document === "undefined") return;
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden && device.platform === "android") void this.refresh();
    });
  }

  /** Whether `path` lies on Android external storage that requires all-files access. */
  isExternal(path: string): boolean {
    return isAndroidExternalPath(path);
  }

  /** Whether a folder at `path` is unreadable because Android all-files access is off. */
  blocked(path: string, offline: Game["offline"] = null): boolean {
    return isAndroidPermissionBlocked({
      platform: device.platform,
      checked: this.checked,
      supported: this.supported,
      granted: this.granted,
      path,
      offline,
    });
  }

  async refresh(): Promise<void> {
    if (!inTauri || device.platform !== "android") return;
    this.#refreshing ??= (async () => {
      try {
        const status = await fetchStorageStatus();
        const changed = this.checked && this.granted !== status.granted;
        this.granted = status.granted;
        this.supported = status.supported;
        this.checked = true;
        if (changed) await library.refresh();
      } catch (error) {
        console.error("storage_status failed", error);
      } finally {
        this.#refreshing = null;
      }
    })();
    return this.#refreshing;
  }

  async request(open = false): Promise<boolean> {
    if (!inTauri || device.platform !== "android") return true;
    this.busy = true;
    try {
      const status = await invokeRequestStorage(open);
      const changed = !this.checked || this.granted !== status.granted;
      this.granted = status.granted;
      this.supported = status.supported;
      this.checked = true;
      if (changed) await library.refresh();
      return status.granted;
    } finally {
      this.busy = false;
    }
  }
}

export const androidStorage = new AndroidStorage();
