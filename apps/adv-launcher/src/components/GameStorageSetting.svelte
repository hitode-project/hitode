<script lang="ts">
  import { onMount } from "svelte";
  import { device } from "../lib/device.svelte";
  import { gameStorage } from "../lib/game-storage.svelte";
  import { storageSize } from "../lib/storage";
  import { t } from "../lib/i18n.svelte";
  import { nav } from "../lib/nav.svelte";

  onMount(() => { void gameStorage.refresh().catch(console.error); });

  const mobile = $derived(device.platform === "android" || device.platform === "ios");
  const visible = $derived(mobile || !gameStorage.measured || gameStorage.total > 0);
</script>

{#if visible}
  <div class="row">
    <span class="t">
      <span>{t("管理游戏存储")}</span>
      {#if gameStorage.orphanBytes > 0}
        <span class="badge">{t("可清理 {0} 未关联项", storageSize(gameStorage.orphanBytes))}</span>
      {/if}
    </span>
    {#if gameStorage.measured}<span class="size">{storageSize(gameStorage.total)}</span>{/if}
    <button class="quiet" aria-label={t("管理游戏存储")} onclick={() => nav.go({ name: "game-storage" })}>{t("打开")}</button>
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
    font-variant-numeric: tabular-nums;
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
