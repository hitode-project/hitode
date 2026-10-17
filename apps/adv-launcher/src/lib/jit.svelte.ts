import { call } from "./bridge";
import { device } from "./device.svelte";
import { t } from "./i18n.svelte";
import { prefs } from "./prefs.svelte";

/** iOS: whether games run translated to native code (IOS-JIT-1, IOS-JIT-2). */
export interface JitStatus {
  ready: boolean;
  error: string | null;
  waiting: boolean;
  task: "enable" | "ddiStatus" | "downloadDdi" | "external" | null;
  liveContainer: boolean;
  pairingFile: boolean;
  stage: string | null;
  /** Whether the developer disk image is downloaded; null until checked. */
  ddi: boolean | null;
}

const errors: Record<string, string> = {
  invalidPairing: "这不是配对文件，请选择 StikDebug 使用的 pairingFile.plist",
  lockdownPairing: "这是 SideStore 等使用的旧格式配对文件，请选择 StikDebug 使用的 pairingFile.plist",
};

const stages: Record<string, string> = {
  starting: "正在启动 JIT 助手…",
  connecting: "正在连接设备，请保持 LocalDevVPN 开启…",
  checkingDdi: "正在检查开发者镜像…",
  downloading: "正在下载开发者镜像…",
  mounting: "正在挂载开发者镜像…",
  attaching: "正在连接调试器…",
  preparing: "正在准备 JIT 代码池…",
  external: "正在等待 StikDebug…",
};

/** A host error, or a code the helper answers, in the user's words. */
export function explainJit(error: string): string {
  if (error.startsWith("LocalDevVPN unreachable")) return t("连不上 LocalDevVPN，请确认它已开启并显示已连接");
  return errors[error] ? t(errors[error]) : error;
}

/**
 * The JIT state the library, the launch screen and settings share. A game
 * started without JIT runs interpreted for its whole run, so a configured
 * launcher asks for JIT as soon as it opens, and again when it returns to the
 * foreground after a failed attempt (the user may just have turned on
 * LocalDevVPN).
 */
class Jit {
  status = $state<JitStatus | null>(null);
  /** The last command error, shown until the next request. */
  requestError = $state("");
  #ddiAsked = false;
  #autoTried = false;
  #timer: ReturnType<typeof setInterval> | null = null;

  /** Only the iOS player has this row; elsewhere the store stays empty. */
  readonly active = $derived(device.platform === "ios" && device.gameView);
  /** A helper request is running; background status checks do not count. */
  readonly busy = $derived(!!this.status?.waiting && this.status.task !== "ddiStatus");
  /** The embedded helper can run: a pairing file is imported and this is not LiveContainer. */
  readonly configured = $derived(!!this.status && this.status.pairingFile && !this.status.liveContainer);

  /** What the row and the banner say about the current state. */
  readonly note = $derived.by(() => {
    const status = this.status;
    if (!status) return "";
    if (this.requestError) return this.requestError;
    if (status.ready) return t("已启用");
    if (this.busy) return t(stages[status.stage ?? "starting"] ?? "正在启动 JIT 助手…");
    if (status.error) return explainJit(status.error);
    return t("未启用");
  });

  /** Starts polling once the host says this is the iOS player. */
  start(): void {
    if (this.#timer) return;
    void this.refresh();
    this.#timer = setInterval(() => {
      if (!this.status?.ready) void this.refresh();
    }, 1000);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") void this.#retryAfterReturn();
    });
  }

  async refresh(): Promise<void> {
    this.status = await call<JitStatus | null>("jit_status", {}, null).catch(() => null);
    const status = this.status;
    if (!status || status.ready || status.liveContainer) return;
    if (status.waiting) return;
    // Enabling checks the image itself, so it goes first.
    if (prefs.autoJit && status.pairingFile && !this.#autoTried) {
      this.#autoTried = true;
      void this.enable();
      return;
    }
    // Ask the helper once per run; the answer stays in the host afterwards.
    if (status.ddi === null && !this.#ddiAsked) {
      this.#ddiAsked = true;
      void call("check_ddi", {}, null).catch(() => {});
    }
  }

  async #retryAfterReturn(): Promise<void> {
    await this.refresh();
    const status = this.status;
    if (prefs.autoJit && this.configured && status && !status.ready && !status.waiting && status.error) void this.enable();
  }

  enable(): Promise<void> {
    return this.#run("request_jit", { external: false });
  }

  stikDebug(): Promise<void> {
    return this.#run("request_jit", { external: true });
  }

  downloadDdi(): Promise<void> {
    return this.#run("download_ddi");
  }

  async importPairing(): Promise<void> {
    this.requestError = "";
    try {
      const imported = await call<boolean>("import_pairing_file", {}, false);
      await this.refresh();
      // A newly configured launcher enables JIT at once, as it would at launch.
      if (imported && prefs.autoJit && this.status && !this.status.ready && !this.status.waiting) void this.enable();
    } catch (error) {
      this.requestError = explainJit(String(error));
    }
  }

  async #run(command: string, args: Record<string, unknown> = {}): Promise<void> {
    this.requestError = "";
    try {
      await call(command, args, null);
      await this.refresh();
    } catch (error) {
      this.requestError = explainJit(String(error));
    }
  }

  /** Resolves once a running request ends; at once when none runs. */
  async settled(): Promise<void> {
    while (this.status?.waiting && this.status.task !== "ddiStatus") {
      await new Promise((resolve) => setTimeout(resolve, 300));
      await this.refresh();
    }
  }
}

export const jit = new Jit();
