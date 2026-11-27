import { call, inTauri, isAclError, onHostEvent } from "./bridge";
import { prefs, savePrefs } from "./prefs.svelte";

export interface Update {
  latest: string | null;
  url: string;
  canInstall?: boolean;
}

export type InstallStage = "idle" | "downloading" | "verifying" | "installing" | "restarting";

interface UpdateProgress {
  stage: Exclude<InstallStage, "idle">;
  downloaded: number;
  total: number | null;
}

const DAY = 24 * 60 * 60 * 1000;

class Updates {
  status = $state<"idle" | "checking" | "failed" | Update>("idle");
  dismissed = $state<string | null>(null);
  installState = $state<InstallStage>("idle");
  progress = $state<{ downloaded: number; total: number | null }>({ downloaded: 0, total: null });
  installError = $state<string | null>(null);

  readonly installing = $derived(this.installState !== "idle");
  readonly percent = $derived(
    this.progress.total && this.progress.total > 0
      ? Math.min(100, Math.round((this.progress.downloaded / this.progress.total) * 100))
      : null,
  );
  readonly notice = $derived(
    (this.installing || prefs.autoCheckUpdates) &&
      typeof this.status === "object" &&
      this.status.latest &&
      this.status.latest !== this.dismissed
      ? this.status
      : null,
  );

  async check(automatic = false): Promise<void> {
    if (this.status === "checking" || this.installing) return;
    const now = Date.now();
    if (automatic && (!inTauri || now - prefs.lastUpdateCheck < DAY)) return;
    const previous = this.status;
    const previousCheck = prefs.lastUpdateCheck;
    this.status = "checking";
    this.installError = null;
    if (inTauri) {
      prefs.lastUpdateCheck = now;
      savePrefs();
    }
    try {
      this.status = (await call<Update | null>("check_update", {}, null)) ?? "failed";
    } catch (error) {
      if (inTauri && isAclError(error)) {
        prefs.lastUpdateCheck = previousCheck;
        savePrefs();
      }
      this.status = automatic ? previous : "failed";
    }
  }

  async install(): Promise<void> {
    if (this.installing || !inTauri) return;
    if (typeof this.status !== "object" || !this.status.canInstall) return;
    this.installError = null;
    this.installState = "downloading";
    this.progress = { downloaded: 0, total: null };
    const stop = onHostEvent<UpdateProgress>("update-progress", (event) => {
      this.installState = event.stage;
      this.progress = { downloaded: event.downloaded, total: event.total };
    });
    try {
      await call<void>("install_update", {}, undefined);
      this.installState = "restarting";
    } catch (error) {
      this.installState = "idle";
      this.installError = error instanceof Error ? error.message : String(error);
    } finally {
      stop();
    }
  }
}

export const updates = new Updates();
