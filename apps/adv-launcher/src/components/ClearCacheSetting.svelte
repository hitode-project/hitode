<script lang="ts">
  import { call, preview } from "../lib/bridge";
  import { t } from "../lib/i18n.svelte";
  import { general } from "../lib/settings-form.svelte";

  let state = $state<"idle" | "clearing" | "cleared" | "error">("idle");

  async function clear() {
    state = "clearing";
    try {
      if (preview) await general.clearCache();
      else await call("clear_game_cache", {}, null);
      state = "cleared";
    } catch {
      state = "error";
    }
  }
</script>

<div class="row">
  <div>
    <span>{t("图形缓存")}</span>
    <p>{t("清除后会自动重建，游戏和存档不受影响。")}</p>
    {#if state === "error"}
      <p class="destructive" role="alert">{t("未能清除缓存，请重试。")}</p>
    {:else}
      <span class="sr" role="status">{state === "cleared" ? t("已清除") : ""}</span>
    {/if}
  </div>
  <button class="quiet" onclick={clear} disabled={state === "clearing" || state === "cleared"}>
    {state === "clearing" ? t("正在清除…") : state === "cleared" ? t("已清除") : t("清除缓存")}
  </button>
</div>

<style>
  .row { display: flex; align-items: center; justify-content: space-between; gap: 12px; min-height: 48px; font-size: 14px; }
  p { margin: 4px 0; color: var(--fg3); font-size: 12px; }
  p.destructive { color: var(--danger); }
  button { flex: none; }
</style>
