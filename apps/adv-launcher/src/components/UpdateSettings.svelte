<script lang="ts">
  import { device } from "../lib/device.svelte";
  import { t } from "../lib/i18n.svelte";
  import { prefs, savePrefs } from "../lib/prefs.svelte";
  import { updates } from "../lib/updates.svelte";
  import SettingsSwitch from "./SettingsSwitch.svelte";

  const id = $props.id();
  const status = $derived(updates.status);

  function setAutomatic(value: boolean): void {
    prefs.autoCheckUpdates = value;
    savePrefs();
  }
</script>

<section aria-labelledby={id}>
  <h2 {id}>{t("软件更新")}</h2>
  <div class="row">
    <div class="label">
      <span>{t("版本")} {device.version}</span>
      <span class="meta" aria-live="polite">
        {#if status === "checking"}{t("正在检查…")}
        {:else if status === "failed"}{t("检查失败，请稍后再试")}
        {:else if status !== "idle" && status.latest}{t("有新版本 {0}", status.latest)} · <a href={status.url} target="_blank" rel="noreferrer">{t("前往下载")}</a>
        {:else if status !== "idle"}{t("已是最新版本")}
        {/if}
      </span>
    </div>
    <button onclick={() => updates.check()} disabled={status === "checking"}>{t("检查更新")}</button>
  </div>
  <div class="row">
    <div class="label">
      <span>{t("自动检查更新")}</span>
      <span class="meta">{t("每 24 小时最多检查一次")}</span>
    </div>
    <SettingsSwitch touch={device.touch} label={t("自动检查更新")} checked={prefs.autoCheckUpdates} onchange={setAutomatic} />
  </div>
</section>

<style>
  h2 {
    margin: 0 0 4px;
    color: var(--fg3);
    font-size: 12px;
    font-weight: 500;
    letter-spacing: 0.06em;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 56px;
    padding: 6px 0;
    border-bottom: 1px solid var(--line);
    box-sizing: border-box;
  }
  .label {
    flex: 1 1 auto;
    min-width: 0;
    display: grid;
    gap: 3px;
    font-size: 14px;
  }
  .meta { color: var(--fg2); font-size: 12px; }
  .meta:empty { display: none; }
  a { color: var(--accent); }
  button {
    flex: 0 0 auto;
    min-height: 36px;
    padding: 0 14px;
    border: 1px solid var(--line);
    border-radius: 18px;
    background: transparent;
    color: var(--accent);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  button:disabled { opacity: 0.6; cursor: default; }
</style>
