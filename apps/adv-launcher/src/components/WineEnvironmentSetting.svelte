<script lang="ts">
  import { onMount } from "svelte";
  import { preview } from "../lib/bridge";
  import { device } from "../lib/device.svelte";
  import { t } from "../lib/i18n.svelte";
  import { nav } from "../lib/nav.svelte";
  import { storageSize } from "../lib/storage";
  import { wineEnv } from "../lib/wine-env.svelte";

  const visible = $derived(device.gameView || preview);

  onMount(() => {
    if (visible) void wineEnv.refresh().catch(console.error);
  });
</script>

{#if visible}
  <div class="row">
    <span class="t">
      <span>{t("Wine 环境")}</span>
      {#if wineEnv.status?.pendingReset.length}
        <span class="badge">{t("待重启执行")}</span>
      {/if}
    </span>
    {#if wineEnv.measured && wineEnv.status}
      <span class="size">{storageSize(wineEnv.status.totalBytes)}</span>
    {/if}
    <button class="quiet" aria-label={t("Wine 环境")} onclick={() => nav.go({ name: "wine-env" })}>{t("打开")}</button>
  </div>
{/if}

<style>
  .row {
    min-height: 56px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid var(--line);
  }
  .size {
    margin-left: auto;
    color: var(--fg3);
    font-size: 13px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .t {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    font-size: 14px;
    color: var(--fg);
  }
  .badge {
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--field);
    color: var(--fg2);
    font-size: 11px;
  }
  .quiet {
    box-sizing: border-box;
    height: 32px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    border-radius: 16px;
    white-space: nowrap;
  }
  :global(:root[data-device="phone"]) .t {
    font-size: 15px;
  }
  :global(:root[data-device="phone"]) .quiet {
    height: 36px;
    border-radius: 18px;
    font-size: 14px;
  }
</style>
