<script lang="ts">
  import { call } from "../lib/bridge";
  import { t } from "../lib/i18n.svelte";

  /** iOS: whether games run translated to native code (IOS-JIT-1, IOS-JIT-2). */
  interface JitStatus {
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

  let status = $state<JitStatus | null>(null);
  let requestError = $state("");
  let ddiAsked = false;

  async function refresh(): Promise<void> {
    status = await call<JitStatus | null>("jit_status", {}, null).catch(() => null);
    // Ask the helper once per run; the answer stays in the host afterwards.
    if (status && !status.ready && !status.liveContainer && status.ddi === null && !status.waiting && !ddiAsked) {
      ddiAsked = true;
      void call("check_ddi", {}, null).catch(() => {});
    }
  }

  // Both the embedded helper and StikDebug report asynchronously.
  $effect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (!status?.ready) void refresh();
    }, 1000);
    return () => clearInterval(timer);
  });

  async function run(command: string, args: Record<string, unknown> = {}): Promise<void> {
    requestError = "";
    try {
      await call(command, args, null);
      await refresh();
    } catch (error) {
      requestError = explain(String(error));
    }
  }

  async function importPairing(): Promise<void> {
    requestError = "";
    try {
      await call<boolean>("import_pairing_file", {}, false);
      await refresh();
    } catch (error) {
      requestError = explain(String(error));
    }
  }

  const errors: Record<string, string> = {
    invalidPairing: "这不是配对文件，请选择 StikDebug 使用的 pairingFile.plist",
    lockdownPairing: "这是 SideStore 等使用的旧格式配对文件，请选择 StikDebug 使用的 pairingFile.plist",
  };

  function explain(error: string): string {
    if (error.startsWith("LocalDevVPN unreachable")) return t("连不上 LocalDevVPN，请确认它已开启并显示已连接");
    return errors[error] ? t(errors[error]) : error;
  }

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

  /** Status-only checks run in the background and show no progress. */
  const busy = $derived(!!status?.waiting && status.task !== "ddiStatus");

  const note = $derived(
    !status
      ? ""
      : status.ready
        ? t("已启用")
        : busy
          ? t(stages[status.stage ?? "starting"] ?? "正在启动 JIT 助手…")
          : status.error
            ? explain(status.error)
            : t("未启用：游戏以解释模式运行，速度较慢"),
  );

  const ddiNote = $derived(
    !status || status.ddi === null
      ? t("开发者镜像：检查中…")
      : status.ddi
        ? t("开发者镜像：已下载")
        : t("开发者镜像：未下载（约 16 MB）。可先在平时的网络或代理下下载，启用 JIT 时再切换到 LocalDevVPN。"),
  );
</script>

{#if status}
  <div class="row jit">
    <span class="t">
      <span>{t("JIT 加速")}</span>
      <span class="note" class:ok={status.ready} role="status">{requestError || note}</span>
      {#if !status.ready && !status.liveContainer}
        <span class="note">{status.pairingFile ? t("配对文件：已导入") : t("配对文件：未导入。请导入 StikDebug 使用的 pairingFile.plist。")}</span>
        <span class="note">{ddiNote}</span>
        <span class="note">{t("启用前请开启 LocalDevVPN 和系统开发者模式。")}</span>
      {/if}
      {#if !status.ready && status.liveContainer}
        <span class="note">{t("在 LiveContainer 中请先打开“使用 LiveContainer 的 Bundle ID”")}</span>
      {/if}
    </span>
    {#if !status.ready}
      <div class="actions">
        {#if !status.liveContainer}
          <button class="secondary" disabled={busy} onclick={importPairing}>{status.pairingFile ? t("更换配对文件") : t("导入配对文件")}</button>
          {#if status.ddi === false}
            <button class="secondary" disabled={busy} onclick={() => run("download_ddi")}>{t("下载开发者镜像")}</button>
          {/if}
          <button class="secondary" disabled={busy || !status.pairingFile} onclick={() => run("request_jit", { external: false })}>{t("启用 JIT")}</button>
        {/if}
        <button class="secondary" disabled={busy} onclick={() => run("request_jit", { external: true })}>{t("用 StikDebug 启用")}</button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .jit {
    min-height: 68px;
    gap: 12px;
    flex-wrap: wrap;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .t {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .note {
    font-size: 12px;
    line-height: 1.4;
    color: var(--fg3);
  }
  .note.ok {
    color: var(--accent);
  }
  .secondary {
    flex: 0 0 auto;
    height: 34px;
    padding: 0 14px;
    font-size: 13px;
  }
</style>
