<script lang="ts">
  import { call } from "../lib/bridge";
  import { t } from "../lib/i18n.svelte";

  /** iOS: whether games run translated to native code (IOS-JIT-1). */
  interface JitStatus {
    ready: boolean;
    error: string | null;
    waiting: boolean;
    liveContainer: boolean;
  }

  let status = $state<JitStatus | null>(null);
  let requestError = $state("");

  async function refresh(): Promise<void> {
    status = await call<JitStatus | null>("jit_status", {}, null).catch(() => null);
  }

  // StikDebug works in the background and returns here; follow it until JIT is ready.
  $effect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (!status?.ready) void refresh();
    }, 1000);
    return () => clearInterval(timer);
  });

  async function request(): Promise<void> {
    requestError = "";
    try {
      await call("request_jit", {}, null);
      await refresh();
    } catch (error) {
      requestError = String(error);
    }
  }

  const note = $derived(
    !status
      ? ""
      : status.ready
        ? t("已启用")
        : status.waiting
          ? t("正在等待 StikDebug…")
          : status.error ?? t("未启用：游戏以解释模式运行，速度较慢"),
  );
</script>

{#if status}
  <div class="row jit">
    <span class="t">
      <span>{t("JIT 加速")}</span>
      <span class="note" class:ok={status.ready} role="status">{requestError || note}</span>
      {#if !status.ready && status.liveContainer}
        <span class="note">{t("在 LiveContainer 中请先打开“使用 LiveContainer 的 Bundle ID”")}</span>
      {/if}
    </span>
    {#if !status.ready}
      <button class="secondary" disabled={status.waiting} onclick={request}>{t("用 StikDebug 启用")}</button>
    {/if}
  </div>
{/if}

<style>
  .jit {
    min-height: 68px;
    gap: 12px;
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
