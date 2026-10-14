import { t } from "./i18n.svelte";
import { call } from "./bridge";
import { libraryPickFolder } from "./host";
export interface Backup { id: string; when: number; kind: string; bytes: number }
interface Info { path: string; backups: Backup[]; autoBackup: boolean; keep: number }
export class Saves {
  info = $state<Info>({ path: "", backups: [], autoBackup: true, keep: 5 });
  error = $state("");
  status = $state("");
  busy = $state(false);
  constructor(readonly id: string) { void this.refresh(); }
  async refresh() {
    try {
      const info = await call<Info | null>("save_info", { id: this.id }, null);
      if (!info) throw new Error(t("存档管理需要桌面启动器"));
      this.info = info;
    } catch (e) { this.error = String(e); }
  }
  async policy(autoBackup: boolean, keep: number) {
    this.busy = true;
    try {
      await call("save_policy", { id: this.id, policy: { autoBackup, keep } }, null);
      this.info.autoBackup = autoBackup;
      this.info.keep = keep;
      this.error = "";
    } catch (e) { this.error = String(e); }
    finally { this.busy = false; }
  }
  async act(action: "backup" | "restore" | "import" | "export", backup: string | null = null) {
    this.busy = true;
    this.status = "";
    this.error = "";
    try {
      const folder = action === "import" || action === "export" ? await libraryPickFolder() : null;
      if ((action === "import" || action === "export") && !folder) return;
      const info = await call<Info | null>("save_action", { id: this.id, action, backup, folder }, null);
      if (!info) throw new Error(t("存档管理需要桌面启动器"));
      this.info = info;
      this.status = action === "restore" ? t("已恢复") : action === "backup" ? t("已备份") : action === "import" ? t("已导入") : t("已导出");
    } catch (e) { this.error = String(e); }
    finally { this.busy = false; }
  }
}
