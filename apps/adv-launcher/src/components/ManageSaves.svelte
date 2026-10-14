<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { untrack } from "svelte";
  import { Saves } from "../lib/saves.svelte";
  import { reveal } from "../lib/reveal";
  import type { Game } from "../lib/types";
  let { game, onclose }: { game: Game; onclose: () => void } = $props();
  const saves = new Saves(untrack(() => game.id));
  let restoring = $state<string | null>(null);
  let importing = $state(false);
</script>
<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-label={t("存档")} tabindex="-1">
    <header><h2>{t("存档")}</h2><button class="quiet" onclick={onclose} disabled={saves.busy}>{t("关闭")}</button></header>
    <p>{game.title}</p>
    <p>{t("备份包含此游戏的数据目录和注册表。恢复或导入会替换当前数据，请先退出游戏。")}</p>
    {#if saves.info.path}<p class="path">{saves.info.path} <button class="quiet" onclick={() => reveal(saves.info.path)}>{t("打开")}</button></p>{/if}
    {#if saves.error}<p role="alert">{saves.error}</p>{/if}
    {#if saves.status}<p role="status">{saves.status}</p>{/if}
    <fieldset disabled={saves.busy || !saves.info.path}>
      <div class="row"><label><input type="checkbox" checked={saves.info.autoBackup} onchange={(e) => saves.policy(e.currentTarget.checked, saves.info.keep)} />{t("退出游戏后自动备份")}</label>
        <label>{t("保留")} <select value={saves.info.keep} onchange={(e) => saves.policy(saves.info.autoBackup, Number(e.currentTarget.value))}>{#each [3, 5, 10] as n}<option value={n}>{n}</option>{/each}</select> {t("个自动备份")}</label>
      </div>
      <div class="list">
        {#each saves.info.backups as backup (backup.id)}
          <div class="row"><span>{new Date(backup.when).toLocaleString()} · {backup.kind === "automatic" ? t("自动") : t("手动")} · {(backup.bytes / 1024).toFixed(0)} KB</span>
          {#if restoring === backup.id}<span>{t("覆盖当前数据？")}</span><button class="quiet" onclick={() => restoring = null}>{t("取消")}</button><button class="primary" onclick={() => { void saves.act("restore", backup.id); restoring = null; }}>{t("恢复")}</button>
          {:else}<button class="quiet" onclick={() => restoring = backup.id}>{t("恢复")}</button>{/if}</div>
        {:else}<p>{t("没有备份")}</p>{/each}
      </div>
      <div class="row"><button class="primary" onclick={() => saves.act("backup")}>{t("立即备份")}</button><button class="quiet" onclick={() => saves.act("export")}>{t("导出到文件夹…")}</button><button class="quiet" onclick={() => importing = true}>{t("导入备份文件夹…")}</button></div>
      {#if importing}<p>{t("导入将覆盖当前数据。请选择包含 snapshot.json 的备份文件夹。")}</p><button class="quiet" onclick={() => importing = false}>{t("取消")}</button><button class="primary" onclick={() => { void saves.act("import"); importing = false; }}>{t("继续导入")}</button>{/if}
    </fieldset>
  </div>
</div>
<style>
  .backdrop { position: fixed; inset: 0; z-index: 90; background: #0005; display: grid; place-items: center; padding: 20px; }
  .dialog { width: min(620px, 100%); max-height: 85vh; overflow: auto; padding: 24px; box-sizing: border-box; border-radius: 14px; background: var(--pop); color: var(--fg); }
  header, .row { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
  .row { padding: 12px 0; }
  .path { overflow-wrap: anywhere; }
  fieldset { border: 0; padding: 0; margin: 0; }
  .list { border-block: 1px solid var(--line); margin: 16px 0; }
  p { line-height: 1.6; }
</style>
