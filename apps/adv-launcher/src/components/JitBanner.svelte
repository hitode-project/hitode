<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { jit } from "../lib/jit.svelte";
  import { nav } from "../lib/nav.svelte";

  const status = $derived(jit.status);
</script>

<!-- iOS library: show JIT readiness and signing restrictions. -->
{#if jit.active && status && !status.ready}
  <div class="banner" role="status">
    {#if jit.signingUnsupported}
      <span class="text"><b>{t("当前签名不支持 JIT")}</b><span>{jit.note}</span></span>
    {:else if jit.busy}
      <span class="text"><b>{t("正在启用 JIT")}</b><span>{jit.note}</span></span>
    {:else}
      <span class="text">
        <b>{t("强烈建议启用 JIT")}</b>
        <span>{status.error || jit.requestError ? jit.note : t("未启用时游戏以解释模式运行，速度会慢很多。")}</span>
      </span>
      {#if status.liveContainer}
        <button class="act" onclick={() => jit.stikDebug()}>{t("用 StikDebug 启用")}</button>
      {:else if status.pairingFile}
        <button class="act" onclick={() => jit.enable()}>{status.error ? t("重试") : t("启用")}</button>
      {:else}
        <button class="act" onclick={() => nav.go({ name: "settings" })}>{t("设置方法")}</button>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .banner {
    flex: 0 0 auto;
    margin: 12px 20px 0;
    padding: 10px 12px;
    border-radius: 12px;
    background: color-mix(in srgb, var(--accent) 14%, var(--bg));
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .text {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    line-height: 1.4;
    color: var(--fg2);
  }
  b {
    font-size: 14px;
    font-weight: 600;
    color: var(--fg);
  }
  .act {
    flex: 0 0 auto;
    height: 32px;
    padding: 0 14px;
    border: none;
    border-radius: 16px;
    background: var(--accent);
    color: #fff;
    font-size: 13px;
  }
</style>
