<script lang="ts">
  import { androidStorage } from "../lib/android-access.svelte";
  import { device } from "../lib/device.svelte";
  import { folderChoice, type FolderChoicePrompt } from "../lib/folder-choice.svelte";
  import { nav } from "../lib/nav.svelte";
  import { t } from "../lib/i18n.svelte";
  import { storageSize } from "../lib/storage";

  let dialog = $state<HTMLDialogElement>();
  let busy = $state(false);
  let denied = $state(false);
  let error = $state("");
  const prompt = $derived(folderChoice.prompt);

  $effect(() => {
    void prompt?.mode;
    busy = false;
    denied = false;
    error = "";
  });

  $effect(() => {
    const active = prompt;
    if (active) return nav.onBack(() => { active.resolve(null); return true; });
  });

  $effect(() => {
    if (!dialog) return;
    if (prompt && !dialog.open) dialog.showModal();
    if (!prompt && dialog.open) dialog.close();
  });

  async function grant(active: FolderChoicePrompt) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      const granted = await androidStorage.request();
      if (folderChoice.prompt !== active) return;
      if (granted) active.resolve("granted");
      else denied = true;
    } catch (cause) {
      if (folderChoice.prompt === active) error = String(cause);
    } finally {
      busy = false;
    }
  }
</script>

<dialog
  bind:this={dialog}
  class:phone={device.phone}
  aria-labelledby="folder-choice-title"
  oncancel={(e) => { e.preventDefault(); if (!busy) prompt?.resolve(null); }}
  onclick={(e) => { if (e.target === dialog && !busy) prompt?.resolve(null); }}
>
  {#if prompt}
    <div class="sheet">
      {#if device.phone}<span class="grab" aria-hidden="true"></span>{/if}
      {#if prompt.mode === "permission"}
        <div class="badge" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round"><path d="M11.5 19H4.5A1.5 1.5 0 0 1 3 17.5v-11A1.5 1.5 0 0 1 4.5 5h4.2l2 2h8.8A1.5 1.5 0 0 1 21 8.5V11"></path><path d="M18 13.2l3.5 1.3v2.7c0 2.2-1.5 3.8-3.5 4.6-2-.8-3.5-2.4-3.5-4.6v-2.7z"></path></svg>
        </div>
        <h2 id="folder-choice-title" class="ttl">{t("允许访问游戏文件夹")}</h2>
        {#if error || denied}
          <p class="line">{error || t("尚未开启。若开关置灰，请在系统“应用信息”右上角允许“受限设置”。")}</p>
        {/if}
        <div class="acts">
          <button class="primary" disabled={busy} onclick={() => void grant(prompt)}>
            <span>{t("去授权")}</span>
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 4h6v6M12 4l-7.5 7.5"></path></svg>
          </button>
          <button class="secondary" disabled={busy} onclick={() => prompt.resolve("copy")}>
            {prompt.bytes !== null && prompt.bytes >= 0
              ? t("复制到 Hitode · {0}", storageSize(prompt.bytes))
              : t("复制到 Hitode")}
          </button>
          <button class="quiet" disabled={busy} onclick={() => prompt.resolve(null)}>{t("取消")}</button>
        </div>
      {:else}
        <div class="badge" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round"><rect x="6" y="2.5" width="12" height="19" rx="2.5"></rect><path d="M12 7.5v7M9 11.5l3 3 3-3"></path></svg>
        </div>
        <h2 id="folder-choice-title" class="ttl">{t("复制到 Hitode")}</h2>
        <p class="line">
          {#if error}
            {error}
          {:else if prompt.bytes === null}
            {t("正在计算大小…")}
          {:else if prompt.bytes >= 0}
            {t("原文件会保留。副本预计额外占用 {0}。", storageSize(prompt.bytes))}
          {/if}
        </p>
        <div class="acts">
          <button class="primary" onclick={() => prompt.resolve("copy")}>{t("复制到 Hitode")}</button>
          <button class="quiet" onclick={() => prompt.resolve(null)}>{t("取消")}</button>
        </div>
      {/if}
    </div>
  {/if}
</dialog>

<style>
  dialog {
    --tint: rgba(55, 103, 214, 0.12);
    --badge: var(--accent);
    --grab: rgba(24, 24, 22, 0.16);
    --sheet: #ffffff;
    box-sizing: border-box;
    margin: auto;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--fg);
    max-width: none;
    max-height: none;
    width: min(440px, calc(100% - 48px));
    overflow: visible;
  }
  :global(:root[data-theme="dark"]) dialog {
    --tint: rgba(91, 134, 232, 0.16);
    --grab: rgba(255, 255, 255, 0.2);
    --sheet: #1e1e20;
  }
  :global(:root[data-theme="dark"][data-accent="blue"]) dialog {
    --badge: #7da0f0;
  }
  dialog::backdrop {
    background: rgba(0, 0, 0, 0.34);
  }
  dialog.phone {
    position: fixed;
    inset: auto 0 0 0;
    margin: 0;
    width: 100%;
  }
  .sheet {
    box-sizing: border-box;
    padding: 32px 28px 20px;
    border-radius: 24px;
    background: var(--sheet);
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.22);
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
  }
  dialog.phone .sheet {
    padding: 10px calc(var(--safe-right) + 24px) max(20px, var(--safe-bottom)) calc(var(--safe-left) + 24px);
    border-radius: 24px 24px 0 0;
    box-shadow: 0 -10px 40px rgba(0, 0, 0, 0.25);
    animation: up 0.28s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  .grab {
    width: 36px;
    height: 5px;
    border-radius: 3px;
    background: var(--grab);
    margin-bottom: 22px;
  }
  .badge {
    width: 56px;
    height: 56px;
    border-radius: 28px;
    display: grid;
    place-items: center;
    background: var(--tint);
    color: var(--badge);
  }
  .ttl {
    margin: 16px 0 0;
    font-size: 20px;
    line-height: 1.35;
    font-weight: 600;
  }
  .line {
    margin: 8px 0 0;
    font-size: 14px;
    line-height: 1.55;
    color: var(--fg2);
    text-wrap: pretty;
  }
  .acts {
    align-self: stretch;
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 28px;
  }
  .acts .primary,
  .acts .secondary {
    height: 48px;
    padding: 0 28px;
    border-radius: 24px;
    font-size: 15px;
    font-weight: 500;
  }
  .acts .quiet {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: center;
    height: 44px;
    padding: 0 16px;
    border-radius: 22px;
    font-size: 14px;
  }
  @keyframes up {
    from { transform: translateY(100%); }
    to { transform: none; }
  }
  @media (prefers-reduced-motion: reduce) {
    dialog.phone .sheet { animation: none; }
  }
</style>
