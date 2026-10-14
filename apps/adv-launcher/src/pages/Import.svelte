<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import ImportEncoding from "../components/ImportEncoding.svelte";
  import ImportFooter from "../components/ImportFooter.svelte";
  import ImportOption from "../components/ImportOption.svelte";
  import { ImportPick } from "../lib/import-flow.svelte";
  import { nav } from "../lib/nav.svelte";

  const pick = new ImportPick();
</script>

<main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
  <div class="col">
    {#if pick.phase === "ready" && pick.inspection}
      <header>
        <h1 lang="ja">{pick.title.trim() || pick.inspection.title}</h1>
        <div class="mono" style="margin-top: 8px">{pick.inspection.folder}</div>
        {#if pick.inspection.inLibrary !== null}<div class="hint" style="margin-top: 6px">{t("这个文件夹里已有游戏")}</div>{/if}
      </header>

      {#if pick.noExe}
        <div class="empty" role="status">
          <div class="msg">{t("没找到程序")}</div>
          <div class="hint">{t("这个文件夹里没有能运行的 32 位 Windows 程序")}</div>
          <button class="quiet" style="margin-left: -12px" onclick={() => void pick.pick()}>{t("重新选择文件夹…")}</button>
        </div>
      {:else}
        <fieldset>
          <legend class="lbl">{t("主程序")}</legend>
          <div class="stack">
            {#each pick.exes as x, i (x.name)}
              <ImportOption name="exe" checked={pick.exe === i} disabled={x.disabled} onpick={() => pick.pickExe(i)}>
                <span class="mono" style="font-size: 14px; color: var(--fg)">{x.name}</span>
                <span class="side-n">{x.disabled ? t("64 位程序") : x.candidate.inLibrary !== null ? t("已在库中") : x.size}</span>
              </ImportOption>
            {/each}
          </div>
        </fieldset>

        <ImportEncoding choices={pick.encs} value={pick.enc} onpick={(v) => pick.pickEnc(v)} />

        <fieldset>
          <legend class="lbl">{t("标题")}</legend>
          <label class="linkf">
            <input type="text" lang="ja" aria-label={t("标题")} value={pick.title} oninput={(e) => pick.setTitle(e.currentTarget.value)} />
          </label>
        </fieldset>
      {/if}
    {:else}
      <div class="status" class:err={pick.phase === "error"} role="status">
        {pick.phase === "error" ? pick.error : pick.phase === "picking" ? t("正在选择文件夹…") : t("正在读取文件夹…")}
      </div>
      {#if pick.phase === "error"}
        <button class="quiet" style="align-self: center" onclick={() => void pick.pick()}>{t("重新选择文件夹…")}</button>
      {/if}
    {/if}
  </div>
</main>

<ImportFooter width={600}>
  <button class="quiet" onclick={() => nav.back()}>{t("取消")}</button>
  <button class="primary" disabled={!pick.addedId && !pick.canAdd} onclick={() => void pick.add()}>{pick.addedId ? t("开始游戏") : pick.chosenInLibrary !== null ? t("在游戏库中查看") : t("添加到游戏库")}</button>
</ImportFooter>

<style>
  .status {
    padding: 48px 0;
    text-align: center;
    font-size: 14px;
    color: var(--fg2);
  }
  .status.err {
    color: var(--danger);
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
  }
  .empty .msg {
    font-size: 15px;
    font-weight: 500;
  }
  .hint {
    font-size: 13px;
    color: var(--fg3);
  }
  .col {
    max-width: 600px;
    margin: 0 auto;
    padding: 64px 32px 40px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 44px;
  }
  h1 {
    margin: 0;
    font-size: 30px;
    line-height: 1.25;
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  fieldset {
    all: unset;
    display: block;
  }
  .lbl {
    margin: 0 0 12px;
    padding: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .side-n {
    font-size: 12px;
    color: var(--fg3);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .linkf {
    display: flex;
    align-items: center;
    height: 44px;
    padding: 0 14px;
    border-radius: 10px;
    background: var(--field);
  }
  .linkf:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .linkf input {
    all: unset;
    flex: 1;
    min-width: 0;
    font-size: 13px;
    color: var(--fg);
  }
  .linkf input::placeholder {
    color: var(--fg3);
  }
</style>
