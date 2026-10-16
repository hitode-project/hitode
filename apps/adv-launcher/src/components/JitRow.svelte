<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { jit } from "../lib/jit.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { setPref } from "../lib/settings-form.svelte";
  import SettingsSwitch from "./SettingsSwitch.svelte";

  const status = $derived(jit.status);
</script>

{#if status}
  <div class="jit">
    <div class="row head">
      <span class="t">
        <span>{t("JIT 加速")}</span>
        <span class="note" class:ok={status.ready} role="status">{jit.note}</span>
      </span>
      {#if !status.ready}
        {#if status.liveContainer}
          <button class="secondary" disabled={jit.busy} onclick={() => jit.stikDebug()}>{t("用 StikDebug 启用")}</button>
        {:else}
          <button class="secondary" disabled={jit.busy || !status.pairingFile} onclick={() => jit.enable()}>{t("启用 JIT")}</button>
        {/if}
      {/if}
    </div>

    {#if !status.ready}
      <p class="advice">{t("强烈建议启用 JIT。未启用时游戏以解释模式运行，速度会慢很多。")}</p>
      {#if status.liveContainer}
        <p class="advice">{t("在 LiveContainer 中请先打开“使用 LiveContainer 的 Bundle ID”")}</p>
      {:else}
        <ol class="steps">
          <li>
            <span class="mark" aria-hidden="true">1</span>
            <span class="what">{t("在“隐私与安全性”中打开开发者模式")}</span>
          </li>
          <li class:done={status.pairingFile}>
            <span class="mark" aria-hidden="true">{status.pairingFile ? "✓" : "2"}</span>
            <span class="what">{status.pairingFile ? t("配对文件已导入") : t("导入 StikDebug 的配对文件 pairingFile.plist")}</span>
            <button class="link" disabled={jit.busy} onclick={() => jit.importPairing()}>{status.pairingFile ? t("更换") : t("导入")}</button>
          </li>
          <li class:done={status.ddi === true}>
            <span class="mark" aria-hidden="true">{status.ddi ? "✓" : "3"}</span>
            <span class="what">
              {status.ddi === null
                ? t("开发者镜像：检查中…")
                : status.ddi
                  ? t("开发者镜像已下载")
                  : t("下载开发者镜像（约 16 MB，启用时也会自动下载）")}
            </span>
            {#if status.ddi === false}
              <button class="link" disabled={jit.busy} onclick={() => jit.downloadDdi()}>{t("下载")}</button>
            {/if}
          </li>
          <li>
            <span class="mark" aria-hidden="true">4</span>
            <span class="what">{t("打开 LocalDevVPN 并确认已连接")}</span>
          </li>
        </ol>
      {/if}
    {/if}

    {#if status.pairingFile && !status.liveContainer}
      <div class="row">
        <span class="t">{t("打开启动器时自动启用 JIT")}</span>
        <SettingsSwitch touch label={t("打开启动器时自动启用 JIT")} checked={prefs.autoJit} onchange={(v) => setPref("autoJit", v)} />
      </div>
    {/if}
  </div>
{/if}

<style>
  /* The settings page's row styles are scoped to it; these match them. */
  .jit {
    display: flex;
    flex-direction: column;
    border-bottom: 1px solid var(--line);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 56px;
  }
  .head {
    min-height: 60px;
  }
  .t {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
    font-size: 15px;
  }
  .note,
  .advice {
    font-size: 12px;
    line-height: 1.4;
    color: var(--fg3);
  }
  .note.ok {
    color: var(--accent);
  }
  .advice {
    margin: 0 0 8px;
  }
  .steps {
    list-style: none;
    margin: 0 0 8px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .steps li {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    line-height: 1.4;
  }
  .mark {
    flex: 0 0 auto;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font-size: 11px;
    border: 1px solid var(--fg3);
    color: var(--fg3);
  }
  .done .mark {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--bg);
  }
  .what {
    flex: 1 1 auto;
    min-width: 0;
  }
  .link {
    flex: 0 0 auto;
    background: none;
    border: none;
    padding: 4px 0;
    font-size: 13px;
    color: var(--accent);
  }
  .secondary {
    flex: 0 0 auto;
    height: 34px;
    padding: 0 14px;
    font-size: 13px;
  }
</style>
