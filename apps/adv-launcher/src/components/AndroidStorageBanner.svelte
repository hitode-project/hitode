<script lang="ts">
  import { androidStorage } from "../lib/android-access.svelte";
  import { t } from "../lib/i18n.svelte";
  import { library } from "../lib/library.svelte";

  const blockedCount = $derived(
    library.games.filter((g) => androidStorage.blocked(g.path, g.offline)).length,
  );
</script>

{#if blockedCount > 0}
  <div class="banner" role="status">
    <span class="text">
      <b>{t("文件访问权限已关闭")}</b>
      <span>{t("{0} 个游戏无法读取", blockedCount)}</span>
    </span>
    <button class="act" disabled={androidStorage.busy} onclick={() => void androidStorage.request()}>
      {t("去开启")}
    </button>
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
    color: var(--on-accent);
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
  }
  .act:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
