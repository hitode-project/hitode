<script lang="ts">
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
      </header>

      {#if pick.noExe}
        <div class="empty" role="status">
          <div class="msg">没找到程序</div>
          <div class="hint">这个文件夹里没有能运行的 32 位 Windows 程序</div>
          <button class="quiet" style="margin-left: -12px" onclick={() => void pick.pick()}>重新选择文件夹…</button>
        </div>
      {:else}
        <fieldset>
          <legend class="lbl">主程序</legend>
          <div class="stack">
            {#each pick.exes as x, i (x.name)}
              <ImportOption name="exe" checked={pick.exe === i} disabled={x.disabled} onpick={() => pick.pickExe(i)}>
                <span class="mono" style="font-size: 14px; color: var(--fg)">{x.name}</span>
                <span class="side-n">{x.disabled ? "64 位程序" : x.size}</span>
              </ImportOption>
            {/each}
          </div>
        </fieldset>

        <ImportEncoding choices={pick.encs} value={pick.enc} onpick={(v) => pick.pickEnc(v)} />

        <fieldset>
          <legend class="lbl">标题</legend>
          <label class="linkf">
            <input type="text" lang="ja" aria-label="标题" value={pick.title} oninput={(e) => pick.setTitle(e.currentTarget.value)} />
          </label>
        </fieldset>
      {/if}
    {:else}
      <div class="status" class:err={pick.phase === "error"} role="status">
        {pick.phase === "error" ? pick.error : pick.phase === "picking" ? "正在选择文件夹…" : "正在读取文件夹…"}
      </div>
      {#if pick.phase === "error"}
        <button class="quiet" style="align-self: center" onclick={() => void pick.pick()}>重新选择文件夹…</button>
      {/if}
    {/if}
  </div>
</main>

<ImportFooter width={600}>
  <button class="quiet" onclick={() => nav.back()}>取消</button>
  <button class="primary" disabled={!pick.addedId && !pick.canAdd} onclick={() => void pick.add()}>{pick.addedId ? "开始游戏" : "添加到游戏库"}</button>
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
  .empty .hint {
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
