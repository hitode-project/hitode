<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import ImportFooter from "../components/ImportFooter.svelte";
  import ImportMini from "../components/ImportMini.svelte";
  import ImportSwitch from "../components/ImportSwitch.svelte";
  import Icon from "../components/Icon.svelte";
  import { depthLabel, SCAN_DEPTHS, SCAN_ENC_OPTIONS, scanLocked, scanNote, ScanPick } from "../lib/import-flow.svelte";
  import { nav } from "../lib/nav.svelte";

  const scan = new ScanPick();
  void scan.begin();
</script>

<main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
  <div class="col">
    <header>
      <h1>{t("添加文件夹")}</h1>
      <div class="mono" style="margin-top: 8px">{scan.root}</div>
    </header>

    <section class="opts">
      <div class="line wrap">
        <span id="lbl-depth" style="flex: 1 1 200px; font-size: 14px">{t("子文件夹层数")}</span>
        <div class="seg" role="group" aria-labelledby="lbl-depth">
          {#each SCAN_DEPTHS as n (n)}
            <button class:on={scan.depth === n} aria-pressed={scan.depth === n} aria-label={depthLabel(n)} onclick={() => scan.setDepth(n)}
              >{n}</button
            >
          {/each}
        </div>
      </div>
      <div class="line">
        <span style="flex: 1 1 auto; font-size: 14px">{t("以后自动发现新游戏")}</span>
        <ImportSwitch checked={scan.watch} label={t("以后自动发现新游戏")} onchange={(v) => (scan.watch = v)} />
      </div>
    </section>

    {#if scan.phase !== "ready"}
      <div class="status" class:err={scan.phase === "error"} role="status">{scan.phase === "error" ? scan.error : scan.phase === "picking" ? t("正在选择文件夹…") : t("正在查找游戏…")}</div>
    {:else}
        <section aria-labelledby="h-found" style="display: flex; flex-direction: column; gap: 10px">
      <div style="display: flex; align-items: baseline; gap: 12px; padding: 0 2px">
        <h2 id="h-found" class="lbl">{t("找到")} {scan.list.length} {t("个")}</h2>
        <div style="flex: 1"></div>
        {#if scan.pickable.length > 0}
          <button class="link" style="font-size: 13px" onclick={() => scan.toggleAll()}>{scan.allOn ? t("全不选") : t("全选")}</button>
        {/if}
      </div>
      {#if scan.list.length === 0}
        <div style="padding: 36px 0; text-align: center; font-size: 14px; color: var(--fg2)">{t("这一层没有游戏")}</div>
      {/if}
      <div style="display: flex; flex-direction: column; gap: 2px">
        {#each scan.list as g (g.id)}
          {@const locked = scanLocked(g)}
          {@const on = scan.isOn(g)}
          <div class="row" class:on class:off={locked && !g.setup}>
            <label class="pick">
              <input type="checkbox" checked={on} disabled={locked} onchange={() => scan.toggle(g)} />
              <span class="box" aria-hidden="true"><Icon name="check" size={12} stroke={2.2} /></span>
              <span style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
                <span lang="ja" class="ell" style="font-size: 14px">{g.name}</span>
                <span class="mono ell" lang="ja">{g.path}</span>
              </span>
            </label>
            <span class="exe mono" style="color: var(--fg2); white-space: nowrap">{g.exe}</span>
            {#if !locked}
              <ImportMini options={SCAN_ENC_OPTIONS} value={scan.encOf(g)} label={t("{0} 的编码", g.name)} onpick={(v) => scan.setEnc(g, v)} />
            {/if}
            {#if g.setup}
              <span class="note" role="status">{t("暂不支持安装")}</span>
            {/if}
            {#if locked && !g.setup}
              <span style="font-size: 12px; color: var(--fg3); white-space: nowrap">{scanNote(g)}</span>
            {/if}
          </div>
        {/each}
      </div>
    </section>
    {/if}
  </div>
</main>

<ImportFooter width={720}>
  <button class="quiet" onclick={() => nav.back()}>{t("取消")}</button>
  <button class="primary" onclick={() => scan.add()} disabled={scan.adding || (!scan.done && scan.chosen.length === 0)}>{scan.addLabel}</button>
</ImportFooter>

<style>
  .status {
    padding: 36px 0;
    text-align: center;
    font-size: 14px;
    color: var(--fg2);
  }
  .status.err {
    color: var(--danger);
  }
  .note {
    font-size: 12px;
    color: var(--fg3);
    white-space: nowrap;
  }
  .col {
    max-width: 720px;
    margin: 0 auto;
    padding: 64px 32px 40px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 36px;
  }
  h1 {
    margin: 0;
    font-size: 30px;
    line-height: 1.25;
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  .opts {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--line);
  }
  .line {
    display: flex;
    align-items: center;
    gap: 20px;
    min-height: 60px;
    border-bottom: 1px solid var(--line);
  }
  .line.wrap {
    flex-wrap: wrap;
  }
  .lbl {
    margin: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .seg {
    --seg-on: #ffffff;
    display: inline-flex;
    padding: 2px;
    border-radius: 9px;
    background: var(--field);
    gap: 2px;
  }
  :global(:root[data-theme="dark"]) .seg {
    --seg-on: #2c2c2f;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    min-width: 40px;
    height: 30px;
    padding: 0 10px;
    display: grid;
    place-items: center;
    border-radius: 7px;
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
    font-variant-numeric: tabular-nums;
  }
  .seg button.on {
    background: var(--seg-on);
    color: var(--fg);
    font-weight: 500;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
  }
  .seg button:focus-visible,
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    align-items: center;
    column-gap: 18px;
    min-height: 60px;
    padding: 0 14px;
    border-radius: 12px;
  }
  .row.on {
    background: var(--sel);
  }
  .row.off {
    opacity: 0.55;
  }
  .pick {
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 0;
    cursor: pointer;
  }
  .pick input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .box {
    flex: 0 0 auto;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    box-sizing: border-box;
    border: 1.5px solid var(--fg3);
    display: grid;
    place-items: center;
    color: var(--on-accent);
  }
  .box :global(svg) {
    opacity: 0;
  }
  .row.on .box {
    background: var(--accent);
    border-color: var(--accent);
  }
  .row.on .box :global(svg) {
    opacity: 1;
  }
  .pick:focus-within .box {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .ell {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  @media (max-width: 760px) {
    .row {
      grid-template-columns: minmax(0, 1fr) auto;
      row-gap: 6px;
      padding: 10px 14px;
    }
    .row .exe {
      display: none;
    }
  }
</style>
