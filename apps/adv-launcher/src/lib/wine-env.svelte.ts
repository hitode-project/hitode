import { call, preview } from "./bridge";
import { gameStorage } from "./game-storage.svelte";

export type WineCleanupTarget = "temp" | "registry" | "windows" | "programs" | "users";

export interface WineEnvironmentStatus {
  totalBytes: number;
  tempBytes: number;
  registryBytes: number;
  windowsBytes: number;
  programsBytes: number;
  usersBytes: number;
  serverStarted: boolean;
  pendingReset: WineCleanupTarget[];
}

export interface WineCleanupResult {
  executed: WineCleanupTarget[];
  scheduled: WineCleanupTarget[];
  freedBytes: number;
  status: WineEnvironmentStatus;
}

const MB = 1_048_576;

class WineEnvironment {
  status = $state<WineEnvironmentStatus | null>(preview ? {
    totalBytes: Math.round((184 + 12.4 + 68.2 + 310 + 37.4) * MB),
    tempBytes: 184 * MB,
    registryBytes: Math.round(12.4 * MB),
    windowsBytes: Math.round(68.2 * MB),
    programsBytes: 310 * MB,
    usersBytes: Math.round(37.4 * MB),
    serverStarted: false,
    pendingReset: [],
  } : null);

  measured = $state(preview);
  #refreshing: Promise<void> | null = null;

  async refresh() {
    if (preview) return;
    this.#refreshing ??= call<WineEnvironmentStatus | null>("wine_environment_status", {}, null)
      .then((value) => {
        if (value) this.status = value;
        this.measured = true;
      })
      .finally(() => { this.#refreshing = null; });
    return this.#refreshing;
  }

  async clean(targets: WineCleanupTarget[]): Promise<WineCleanupResult | null> {
    if (preview && this.status) {
      const next = { ...this.status };
      let freed = 0;
      const executed: WineCleanupTarget[] = [];
      const scheduled: WineCleanupTarget[] = [];
      for (const target of targets) {
        if (!next.serverStarted || target === "temp") {
          executed.push(target);
          if (target === "temp") { freed += next.tempBytes; next.tempBytes = 0; }
          if (target === "registry") { freed += next.registryBytes; next.registryBytes = 0; }
          if (target === "windows") { freed += next.windowsBytes; next.windowsBytes = 0; }
          if (target === "programs") { freed += next.programsBytes; next.programsBytes = 0; }
          if (target === "users") { freed += next.usersBytes; next.usersBytes = 0; }
        } else {
          scheduled.push(target);
        }
      }
      next.totalBytes = next.tempBytes + next.registryBytes + next.windowsBytes + next.programsBytes + next.usersBytes;
      if (scheduled.length) {
        next.pendingReset = Array.from(new Set([...next.pendingReset, ...scheduled]));
      }
      this.status = next;
      return { executed, scheduled, freedBytes: freed, status: next };
    }
    const result = await call<WineCleanupResult | null>("clean_wine_environment", { targets }, null);
    if (result) {
      this.status = result.status;
      this.measured = true;
    }
    await gameStorage.refreshSummary();
    return result;
  }

  async cancelPending() {
    if (preview && this.status) {
      this.status = { ...this.status, pendingReset: [] };
      return;
    }
    const status = await call<WineEnvironmentStatus | null>("cancel_wine_reset", {}, null);
    if (status) this.status = status;
  }

  async restart() {
    if (preview) return;
    await call("restart_launcher", {}, undefined);
  }
}

export const wineEnv = new WineEnvironment();
