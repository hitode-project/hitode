<script lang="ts">
  import { onMount } from "svelte";
  import { device } from "../lib/device.svelte";
  import { gameStorage } from "../lib/game-storage.svelte";
  import { t } from "../lib/i18n.svelte";
  import { storageSize } from "../lib/storage";

  onMount(() => { void gameStorage.refresh().catch(console.error); });

  const summary = $derived(gameStorage.summary);
  const gameFiles = $derived(summary?.gameFilesBytes ?? 0);
  const envBytes = $derived(summary?.envBytes ?? 0);
  const cacheBytes = $derived(summary?.cacheBytes ?? 0);
  const totalBytes = $derived(gameFiles + envBytes + cacheBytes);
  const denom = $derived(Math.max(totalBytes, 1));
  const filesPct = $derived(gameFiles > 0 ? Math.max(2, (gameFiles / denom) * 100) : 0);
  const envPct = $derived(envBytes > 0 ? Math.max(2, (envBytes / denom) * 100) : 0);
  const cachePct = $derived(cacheBytes > 0 ? Math.max(2, (cacheBytes / denom) * 100) : 0);
</script>

{#if summary}
  <div class="store">
    <div class="head">
      <span class="total">{storageSize(totalBytes)}</span>
      {#if !device.touch && summary.dataDir}
        <span class="datadir" title={summary.dataDir}>{summary.dataDir}</span>
      {/if}
    </div>
    <div
      class="usage"
      role="img"
      aria-label={t("总计 {0}：游戏文件 {1}，数据与运行环境 {2}，缓存 {3}", storageSize(totalBytes), storageSize(gameFiles), storageSize(envBytes), storageSize(cacheBytes))}
    >
      {#if gameFiles > 0}<span class="files" style:width="{filesPct}%"></span>{/if}
      {#if envBytes > 0}<span class="env" style:width="{envPct}%"></span>{/if}
      {#if cacheBytes > 0}<span class="cache" style:width="{cachePct}%"></span>{/if}
    </div>
    <div class="legend">
      {#if gameFiles > 0}
        <span class="key"><i class="files"></i>{t("游戏文件")} {storageSize(gameFiles)}</span>
      {/if}
      <span class="key"><i class="env"></i>{t("数据与运行环境")} {storageSize(envBytes)}</span>
      <span class="key"><i class="cache"></i>{t("缓存")} {storageSize(cacheBytes)}</span>
    </div>
  </div>
{/if}

<style>
  .store {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px 0 18px;
    border-bottom: 1px solid var(--line);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 12px;
    min-width: 0;
  }
  .total {
    flex: 1;
    font-size: 22px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .datadir {
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .usage {
    display: flex;
    height: 6px;
    border-radius: 3px;
    overflow: hidden;
    background: var(--field);
    gap: 2px;
  }
  .usage span {
    transition: width 0.3s ease;
  }
  .files {
    background: var(--accent);
  }
  .env {
    background: var(--fg2);
    opacity: 0.65;
  }
  .cache {
    background: var(--fg3);
    opacity: 0.45;
  }
  .legend {
    display: flex;
    gap: 16px;
    row-gap: 8px;
    flex-wrap: wrap;
  }
  .key {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--fg2);
    font-variant-numeric: tabular-nums;
  }
  .key i {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    display: block;
  }
  @media (prefers-reduced-motion: reduce) {
    .usage span {
      transition: none;
    }
  }
</style>
