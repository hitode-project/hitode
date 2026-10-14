<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  /** Called once with the user's choice; dismissing chooses manual lookup. */
  let { onchoose }: { onchoose: (automatic: boolean) => void } = $props();
  let dialog = $state<HTMLElement>();
  $effect(() => dialog?.focus());
</script>
<div class="backdrop">
  <div
    bind:this={dialog}
    role="dialog"
    aria-modal="true"
    aria-labelledby="vndb-consent-title"
    tabindex="-1"
    onkeydown={(event) => { if (event.key === "Escape") onchoose(false); }}
  >
    <h2 id="vndb-consent-title">{t("自动获取游戏信息？")}</h2>
    <p>{t("Hitode 可以在你添加游戏时，把游戏名发送到 VNDB（vndb.org），自动获取封面、简介和开发商。只发送游戏名，不发送文件、路径或游玩记录。")}</p>
    <p class="hint">{t("你可以随时在设置中更改，也可以为单个游戏手动查询。")}</p>
    <div class="actions">
      <button type="button" class="quiet" onclick={() => onchoose(false)}>{t("仅手动查询")}</button>
      <button type="button" class="primary" onclick={() => onchoose(true)}>{t("自动获取")}</button>
    </div>
  </div>
</div>
<style>
  .backdrop { position: fixed; inset: 0; z-index: 100; background: #0005; display: grid; place-items: center; }
  [role="dialog"] { background: var(--pop); color: var(--fg); padding: 24px; border-radius: 12px; width: min(420px, 85vw); outline: none; }
  h2 { margin: 0 0 12px; }
  p { margin: 0 0 8px; line-height: 1.5; }
  .hint { opacity: 0.7; }
  .actions { display: flex; justify-content: end; gap: 8px; margin-top: 20px; }
</style>
