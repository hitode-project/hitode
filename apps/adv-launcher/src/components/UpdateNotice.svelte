<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { updates } from "../lib/updates.svelte";

  const release = $derived(updates.notice);
</script>

{#if release}
  <div class="notice">
    <span role="status">{t("有新版本 {0}", release.latest ?? "")}</span>
    <a href={release.url} target="_blank" rel="noreferrer">{t("前往下载")}</a>
    <button aria-label={t("关闭")} onclick={() => (updates.dismissed = release.latest)}>
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <path d="m4.5 4.5 7 7m0-7-7 7" fill="none" stroke="currentColor" stroke-linecap="round" stroke-width="1.5" />
      </svg>
    </button>
  </div>
{/if}

<style>
  .notice {
    flex: 0 0 auto;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 12px;
    margin: 8px 20px 0;
    padding: 2px 6px 2px 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: color-mix(in srgb, var(--accent) 5%, var(--bg));
    color: var(--fg);
    font-size: 13px;
  }
  span { margin-inline-end: auto; font-weight: 500; padding: 6px 0; }
  a { color: var(--accent); font-weight: 500; white-space: nowrap; }
  button {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    padding: 0;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--fg2);
    cursor: pointer;
  }
  button:hover { background: var(--line); }
</style>
