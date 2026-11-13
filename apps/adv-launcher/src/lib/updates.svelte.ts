import { call, inTauri, isAclError } from "./bridge";
import { prefs, savePrefs } from "./prefs.svelte";

interface Update {
  latest: string | null;
  url: string;
}

const DAY = 24 * 60 * 60 * 1000;

class Updates {
  status = $state<"idle" | "checking" | "failed" | Update>("idle");
  dismissed = $state<string | null>(null);
  readonly notice = $derived(
    prefs.autoCheckUpdates && typeof this.status === "object" && this.status.latest && this.status.latest !== this.dismissed
      ? this.status
      : null,
  );

  async check(automatic = false): Promise<void> {
    if (this.status === "checking") return;
    const now = Date.now();
    if (automatic && (!inTauri || now - prefs.lastUpdateCheck < DAY)) return;
    const previous = this.status;
    const previousCheck = prefs.lastUpdateCheck;
    this.status = "checking";
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
}

export const updates = new Updates();
