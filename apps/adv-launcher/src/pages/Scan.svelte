<script lang="ts">
  import ImportFooter from "../components/ImportFooter.svelte";
  import ImportMini from "../components/ImportMini.svelte";
  import ImportSwitch from "../components/ImportSwitch.svelte";
  import Icon from "../components/Icon.svelte";
  import { addScanned, chooseInstallSource, scanFolder, SCAN_ROOT, type Enc, type ScanFound } from "../lib/fake-import";
  import { nav } from "../lib/nav.svelte";

  const found = scanFolder();
  const encOptions: { value: Enc; label: string }[] = [
    { value: "ja", label: "日文" },
    { value: "zh", label: "简中" },
  ];

  let depth = $state(2);
  let watch = $state(true);
  /** Results the user unticked. */
  let off = $state<Record<string, boolean>>({});
  /** Encodings the user changed from the detected one. */
  let enc = $state<Record<string, Enc>>({});
  let done = $state(false);

  const list = $derived(found.filter((g) => g.depth <= depth));
  const isLocked = (g: ScanFound) => !!g.inLib || !!g.noExe || !!g.setup;
  const pickable = $derived(list.filter((g) => !isLocked(g)));
  const chosen = $derived(pickable.filter((g) => !off[g.id]));
  const allOn = $derived(chosen.length === pickable.length);
  const encOf = (g: ScanFound): Enc => enc[g.id] ?? g.enc ?? "ja";

  function toggle(g: ScanFound) {
    off[g.id] = !off[g.id];
    done = false;
  }

  function toggleAll() {
    const next = { ...off };
    for (const g of pickable) next[g.id] = allOn;
    off = next;
    done = false;
  }

  function install(g: ScanFound) {
    chooseInstallSource(g.id);
    nav.go({ name: "install" });
  }

  function add() {
    if (!done) {
      addScanned(chosen.map((g) => ({ found: g, enc: encOf(g) })));
      done = true;
    }
  }

  // After the "已添加" confirmation, return to the library.
  $effect(() => {
    if (!done) return;
    const timer = setTimeout(() => nav.home(), 900);
    return () => clearTimeout(timer);
  });
</script>

<main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
  <div class="col">
    <header>
      <h1>添加文件夹</h1>
      <div class="mono" style="margin-top: 8px">{SCAN_ROOT}</div>
    </header>

    <section class="opts">
      <div class="line wrap">
        <span id="lbl-depth" style="flex: 1 1 200px; font-size: 14px">子文件夹层数</span>
        <div class="seg" role="group" aria-labelledby="lbl-depth">
          {#each [0, 1, 2, 3, 4] as n (n)}
            <button
              class:on={depth === n}
              aria-pressed={depth === n}
              aria-label={n === 0 ? "只看所选文件夹" : `${n} 层子文件夹`}
              onclick={() => {
                depth = n;
                done = false;
              }}>{n}</button
            >
          {/each}
        </div>
      </div>
      <div class="line">
        <span style="flex: 1 1 auto; font-size: 14px">以后自动发现新游戏</span>
        <ImportSwitch checked={watch} label="以后自动发现新游戏" onchange={(v) => (watch = v)} />
      </div>
    </section>

    <section aria-labelledby="h-found" style="display: flex; flex-direction: column; gap: 10px">
      <div style="display: flex; align-items: baseline; gap: 12px; padding: 0 2px">
        <h2 id="h-found" class="lbl">找到 {list.length} 个</h2>
        <div style="flex: 1"></div>
        {#if pickable.length > 0}
          <button class="link" style="font-size: 13px" onclick={toggleAll}>{allOn ? "全不选" : "全选"}</button>
        {/if}
      </div>
      {#if list.length === 0}
        <div style="padding: 36px 0; text-align: center; font-size: 14px; color: var(--fg2)">这一层没有游戏</div>
      {/if}
      <div style="display: flex; flex-direction: column; gap: 2px">
        {#each list as g (g.id)}
          {@const locked = isLocked(g)}
          {@const on = !locked && !off[g.id]}
          <div class="row" class:on class:off={locked && !g.setup}>
            <label class="pick">
              <input type="checkbox" checked={on} disabled={locked} onchange={() => toggle(g)} />
              <span class="box" aria-hidden="true"><Icon name="check" size={12} stroke={2.2} /></span>
              <span style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
                <span lang="ja" class="ell" style="font-size: 14px">{g.name}</span>
                <span class="mono ell" lang="ja">{g.path}</span>
              </span>
            </label>
            <span class="exe mono" style="color: var(--fg2); white-space: nowrap">{g.exe}</span>
            {#if !locked}
              <ImportMini options={encOptions} value={encOf(g)} label="{g.name} 的编码" onpick={(v) => { enc[g.id] = v; done = false; }} />
            {/if}
            {#if g.setup}
              <button class="link" style="font-size: 13px" aria-label="安装 {g.name}" onclick={() => install(g)}>安装…</button>
            {/if}
            {#if locked && !g.setup}
              <span style="font-size: 12px; color: var(--fg3); white-space: nowrap">{g.inLib ? "已在游戏库" : "未找到主程序"}</span>
            {/if}
          </div>
        {/each}
      </div>
    </section>
  </div>
</main>

<ImportFooter width={720}>
  <button class="quiet" onclick={() => nav.back()}>取消</button>
  <button class="primary" onclick={add} disabled={!done && chosen.length === 0}>
    {done ? "已添加" : chosen.length ? `添加 ${chosen.length} 个游戏` : "添加"}
  </button>
</ImportFooter>

<style>
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
