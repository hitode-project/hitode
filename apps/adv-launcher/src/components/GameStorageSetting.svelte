<script lang="ts">
  import { onMount } from "svelte";
  import { gameStorage } from "../lib/game-storage.svelte";
  import { storageSize } from "../lib/storage";
  import { t } from "../lib/i18n.svelte";
  import { nav } from "../lib/nav.svelte";

  onMount(() => { void gameStorage.refresh().catch(console.error); });
</script>

<div class="row">
  <span class="t">{t("管理游戏存储")}</span>
  {#if gameStorage.measured}<span class="size">{storageSize(gameStorage.total)}</span>{/if}
  <button class="quiet" aria-label={t("管理游戏存储")} onclick={() => nav.go({ name: "game-storage" })}>{t("打开")}</button>
</div>

<style>
  .row { min-height: 38px; display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .size { margin-left: auto; color: var(--fg3); font-size: 13px; }
  .t { flex: 1; font-size: 14px; color: var(--fg); }
  :global(:root[data-device="phone"]) .row { min-height: 48px; }
</style>
