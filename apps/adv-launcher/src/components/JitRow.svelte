<script lang="ts">
  import { call } from "../lib/bridge";
  import { t } from "../lib/i18n.svelte";

  /** iOS: whether games run translated to native code (IOS-JIT-1). */
  interface JitStatus {
    ready: boolean;
    error: string | null;
    waiting: boolean;
    liveContainer: boolean;
    pairingFile: boolean;
    stage: string | null;
  }

  let status = $state<JitStatus | null>(null);
  let requestError = $state("");

  async function refresh(): Promise<void> {
    status = await call<JitStatus | null>("jit_status", {}, null).catch(() => null);
  }

  // Both the embedded helper and StikDebug report asynchronously.
  $effect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (!status?.ready) void refresh();
    }, 1000);
    return () => clearInterval(timer);
  });

  async function request(external = false): Promise<void> {
    requestError = "";
    try {
      await call("request_jit", { external }, null);
      await refresh();
    } catch (error) {
      requestError = String(error);
    }
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

  const note = $derived(
    !status
      ? ""
      : status.ready
        ? t("已启用")
        : status.waiting
          ? t(stages[status.stage ?? "starting"] ?? "正在启动 JIT 助手…")
          : status.error ?? t("未启用：游戏以解释模式运行，速度较慢"),
  );
</script>

{#if status}
  <div class="row jit">
    <span class="t">
      <span>{t("JIT 加速")}</span>
      <span class="note" class:ok={status.ready} role="status">{requestError || note}</span>
      {#if !status.ready && !status.liveContainer}
        <span class="note">{t("请开启 LocalDevVPN 和系统开发者模式。首次启用需要联网下载开发者镜像。")}</span>
        {#if !status.pairingFile}
          <span class="note">{t("在“文件”App 中将 StikDebug 的 pairingFile.plist 复制到 Hitode 文件夹。")}</span>
        {/if}
      {/if}
      {#if !status.ready && status.liveContainer}
        <span class="note">{t("在 LiveContainer 中请先打开“使用 LiveContainer 的 Bundle ID”")}</span>
      {/if}
    </span>
    {#if !status.ready}
      <div class="actions">
        {#if !status.liveContainer}
          <button class="secondary" disabled={status.waiting || !status.pairingFile} onclick={() => request()}>{t("启用 JIT")}</button>
        {/if}
        <button class="secondary" disabled={status.waiting} onclick={() => request(true)}>{t("用 StikDebug 启用")}</button>
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
