<script lang="ts">
  import { folderChoice } from "../lib/folder-choice.svelte";
  import { nav } from "../lib/nav.svelte";
  import { t } from "../lib/i18n.svelte";
  import { storageSize } from "../lib/storage";

  let dialog = $state<HTMLDialogElement>();
  const prompt = $derived(folderChoice.prompt);
  $effect(() => {
    const active = prompt;
    if (active) return nav.onBack(() => { active.resolve(null); return true; });
  });
  $effect(() => {
    if (!dialog) return;
    if (prompt && !dialog.open) dialog.showModal();
    if (!prompt && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} aria-labelledby="folder-choice-title" oncancel={(e) => { e.preventDefault(); prompt?.resolve(null); }}>
  {#if prompt}
    <h2 id="folder-choice-title">{t(prompt.bytes === null ? "选择游戏存放方式" : "复制到 Hitode")}</h2>
    {#if prompt.bytes === null}
      <div class="actions">
        <button class="primary" onclick={() => prompt?.resolve("original")}>{t("从原位置运行")}</button>
        <button class="secondary" onclick={() => prompt?.resolve("copy")}>{t("复制到 Hitode")}</button>
      </div>
      <button class="quiet cancel" onclick={() => prompt?.resolve(null)}>{t("取消")}</button>
    {:else}
      {#if prompt.forced}<p>{t("此位置无法直接运行，需要复制到 Hitode。")}</p>{/if}
      <p>{t("原文件会保留。副本预计额外占用 {0}。", storageSize(prompt.bytes))}</p>
      <div class="actions">
        <button class="secondary" onclick={() => prompt?.resolve(null)}>{t("取消")}</button>
        <button class="primary" onclick={() => prompt?.resolve("copy")}>{t("复制到 Hitode")}</button>
      </div>
    {/if}
  {/if}
</dialog>

<style>
  dialog { box-sizing: border-box; width: min(410px, calc(100% - 28px)); color: var(--fg); background: var(--pop); border: 1px solid var(--line); border-radius: 14px; padding: 18px; text-align: center; }
  dialog::backdrop { background: #0005; }
  h2 { margin: 0 0 12px; font-size: 17px; }
  p { color: var(--fg2); margin: 6px 0; }
  .actions { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; margin-top: 16px; }
  .actions button { min-height: 44px; padding-inline: 8px; justify-content: center; white-space: normal; }
  .cancel { min-height: 44px; margin-top: 6px; }
</style>
