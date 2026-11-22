<script lang="ts">
  import { gameStorage } from "../lib/game-storage.svelte";
  import { t } from "../lib/i18n.svelte";
  import { storageSize } from "../lib/storage";

  let state = $state<"idle" | "clearing" | "error">("idle");
  const cacheBytes = $derived(gameStorage.summary?.cacheBytes ?? null);
  const empty = $derived(cacheBytes !== null && cacheBytes === 0);

  async function clear() {
    if (state === "clearing" || empty) return;
    state = "clearing";
    try {
      await gameStorage.clearCache();
      state = "idle";
    } catch {
      state = "error";
    }
  }
</script>

<div class="row">
  <span class="t">{t("缓存")}</span>
  {#if cacheBytes !== null}
    <span class="size">{cacheBytes > 0 ? storageSize(cacheBytes) : t("无缓存")}</span>
  {/if}
  <button class="quiet" onclick={clear} disabled={state === "clearing" || empty}>
    {state === "clearing" ? t("正在清除…") : t("清除")}
  </button>
</div>
{#if state === "error"}
  <p class="destructive" role="alert">{t("未能清除缓存，请重试。")}</p>
{/if}

<style>
  .row {
    min-height: 56px;
    display: flex;
    align-items: center;
    gap: 12px;
    border-bottom: 1px solid var(--line);
  }
  .t {
    flex: 1;
    font-size: 14px;
    color: var(--fg);
  }
  .size {
    color: var(--fg3);
    font-size: 13px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  button {
    flex: none;
    box-sizing: border-box;
    height: 32px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    border-radius: 16px;
    white-space: nowrap;
  }
  button[disabled] {
    cursor: default;
    color: var(--fg3);
    background: transparent;
  }
  p.destructive {
    margin: 6px 0 0;
    color: var(--danger);
    font-size: 12px;
  }
  :global(:root[data-device="phone"]) .t {
    font-size: 15px;
  }
  :global(:root[data-device="phone"]) button {
    height: 36px;
    border-radius: 18px;
    font-size: 14px;
  }
</style>
