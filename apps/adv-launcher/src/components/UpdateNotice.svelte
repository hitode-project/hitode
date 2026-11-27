<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { updates } from "../lib/updates.svelte";

  const release = $derived(updates.notice);
  const progressText = $derived.by(() => {
    switch (updates.installState) {
      case "downloading":
        return updates.percent !== null ? t("正在下载更新… {0}%", updates.percent) : t("正在下载更新…");
      case "verifying":
        return t("正在校验签名…");
      case "installing":
        return t("正在安装更新…");
      case "restarting":
        return t("正在重启…");
      default:
        return "";
    }
  });
</script>

{#if release}
  <div class="notice">
    <span role="status">
      {#if updates.installing}{progressText}
      {:else if updates.installError}{t("自动更新失败，请重试或手动下载")}
      {:else}{t("有新版本 {0}", release.latest ?? "")}
      {/if}
    </span>
    {#if release.canInstall && !updates.installing}
      <button class="action" onclick={() => updates.install()}>
        {updates.installError ? t("重新更新") : t("立即更新")}
      </button>
    {/if}
    {#if !updates.installing}
      <a href={release.url} target="_blank" rel="noreferrer">
        {release.canInstall && !updates.installError ? t("更新说明") : t("前往下载")}
      </a>
      <button class="close" aria-label={t("关闭")} onclick={() => (updates.dismissed = release.latest)}>
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
          <path d="m4.5 4.5 7 7m0-7-7 7" fill="none" stroke="currentColor" stroke-linecap="round" stroke-width="1.5" />
        </svg>
      </button>
    {/if}
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
  .action {
    min-height: 28px;
    padding: 0 12px;
    border: 1px solid var(--accent);
    border-radius: 14px;
    background: var(--accent);
    color: #fff;
    font: inherit;
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
    cursor: pointer;
  }
  .action:hover { filter: brightness(1.06); }
  .close {
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
  .close:hover { background: var(--line); }
</style>
