<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { untrack } from "svelte";
  import { library } from "../lib/library.svelte";
  import type { Game } from "../lib/types";
  let { game, onclose }: { game: Game; onclose: () => void } = $props();
  let title = $state(untrack(() => game.title));
  let error = $state("");
  let saving = $state(false);
  async function save(event: SubmitEvent) {
    event.preventDefault();
    saving = true;
    try { await library.rename(game.id, title.trim()); onclose(); }
    catch (e) { error = String(e); }
    finally { saving = false; }
  }
</script>
<div class="backdrop">
  <div role="dialog" aria-modal="true" aria-label={t("编辑标题")} tabindex="-1"><form onsubmit={save}>
    <h2>{t("编辑标题")}</h2>
    <label>{t("标题")} <input bind:value={title} required /></label>
    {#if error}<p role="alert">{error}</p>{/if}
    <div class="actions">
      <button type="button" class="quiet" onclick={onclose} disabled={saving}>{t("取消")}</button>
      <button class="primary" disabled={saving || !title.trim()}>{t("保存")}</button>
    </div>
  </form></div>
</div>
<style>
  .backdrop { position: fixed; inset: 0; z-index: 100; background: #0005; display: grid; place-items: center; }
  form { background: var(--pop); color: var(--fg); padding: 24px; border-radius: 12px; width: min(400px, 85vw); }
  input { width: 100%; margin-top: 12px; padding: 8px; box-sizing: border-box; }
  .actions { display: flex; justify-content: end; gap: 8px; margin-top: 20px; }
</style>
