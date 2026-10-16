<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import ScanExe from "../components/ScanExe.svelte";
  import Icon from "../components/Icon.svelte";
  import PhoneImportFooter from "../components/PhoneImportFooter.svelte";
  import PhoneImportHeader from "../components/PhoneImportHeader.svelte";
  import PhoneImportMini from "../components/PhoneImportMini.svelte";
  import PhoneImportPage from "../components/PhoneImportPage.svelte";
  import PhoneImportPermission from "../components/PhoneImportPermission.svelte";
  import PhoneImportSwitch from "../components/PhoneImportSwitch.svelte";
  import { device } from "../lib/device.svelte";
  import { hasStoragePermission } from "../lib/storage";
  import { depthLabel, SCAN_DEPTHS, SCAN_ENC_OPTIONS, scanLocked, scanNote, ScanPick } from "../lib/import-flow.svelte";
  import { nav } from "../lib/nav.svelte";

  const ios = $derived(device.platform === "ios");
  // Storage access comes first: on iOS every folder is picked through Files, on
  // Android once the permission is granted the folder picker opens directly.
  let asking = $state(true);
  let checking = $state(true);
  let permissionError = $state("");

  const scan = new ScanPick();
  $effect(() => {
    const platform = device.platform;
    let active = true;
    checking = true;
    void hasStoragePermission().then((granted) => {
      if (active) { asking = platform === "ios" || !granted; checking = false; }
    }, (error) => {
      if (active) { permissionError = String(error); checking = false; }
    });
    return () => { active = false; };
  });
  // The folder picker opens once storage access is settled.
  $effect(() => {
    if (!asking) void scan.begin();
  });
</script>

<PhoneImportPage>
  <div class="base" inert={asking}>
    <PhoneImportHeader label={t("取消")} onclose={() => nav.back()} />

    <main class="scroll body">
      <div>
        <h1>{t("添加文件夹")}</h1>
        {#if !asking}<div class="mono" style="margin-top: 6px">{scan.root}</div>{/if}
      </div>

      {#if !asking}
        <div style="display: flex; flex-direction: column; gap: 10px">
          <div class="lbl" id="p-lbl-depth">{t("子文件夹层数")}</div>
          <div class="seg" role="group" aria-labelledby="p-lbl-depth">
            {#each SCAN_DEPTHS as n (n)}
              <button class:on={scan.depth === n} aria-pressed={scan.depth === n} aria-label={depthLabel(n)} onclick={() => scan.setDepth(n)}
                >{n}</button
              >
            {/each}
          </div>
          <div class="watch">
            <span style="flex: 1; font-size: 15px">{t("以后自动发现新游戏")}</span>
            <PhoneImportSwitch checked={scan.watch} label={t("以后自动发现新游戏")} onchange={(v) => (scan.watch = v)} />
          </div>
        </div>

        {#if scan.phase !== "ready"}
          <div class="status" class:err={scan.phase === "error"} role="status">{scan.phase === "error" ? scan.error : scan.copyStatus || (scan.phase === "picking" ? t("正在选择文件夹…") : t("正在查找游戏…"))}</div>
        {:else}
                <section aria-labelledby="p-h-found" style="display: flex; flex-direction: column; gap: 6px">
          <div style="display: flex; align-items: center; gap: 12px">
            <h2 id="p-h-found" class="lbl" style="margin: 0; flex: 1">{t("找到")} {scan.list.length} {t("个")}</h2>
            {#if scan.pickable.length > 0}
              <button class="link" onclick={() => scan.toggleAll()}>{scan.allOn ? t("全不选") : t("全选")}</button>
            {/if}
          </div>
          {#if scan.list.length === 0}
            <div style="padding: 32px 0; text-align: center; font-size: 14px; color: var(--fg2)">{t("这一层没有游戏")}</div>
          {/if}
          {#each scan.list as g (g.id)}
            {@const locked = scanLocked(g)}
            {@const on = scan.isOn(g)}
            <div class="row" class:on class:off={locked && !g.setup}>
              <label class="pick">
                <input type="checkbox" checked={on} disabled={locked} onchange={() => scan.toggle(g)} />
                <span class="box" aria-hidden="true"><Icon name="check" size={12} stroke={2.2} /></span>
                <span style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
                  <span lang="ja" class="ell" style="font-size: 15px">{g.name}</span>
                  <span class="mono ell" lang="ja">{g.path}</span>
                </span>
              </label>
              <div class="sub">
                <span style="flex: 1; min-width: 0; display: flex">
                  {#if locked}
                    <span class="mono ell" style="color: var(--fg2)">{g.exe}</span>
                  {:else}
                    <ScanExe phone name={g.name} exes={g.exes} value={scan.exeOf(g)?.path ?? g.exe} onpick={(v) => scan.setExe(g, v)} />
                  {/if}
                </span>
                {#if !locked}
                  <PhoneImportMini options={SCAN_ENC_OPTIONS} value={scan.encOf(g)} label={t("{0} 的编码", g.name)} onpick={(v) => scan.setEnc(g, v)} />
                {/if}
                {#if g.setup}
              <span class="note" role="status">{t("暂不支持安装")}</span>
                {/if}
                {#if locked && !g.setup}
                  <span style="font-size: 12px; color: var(--fg3); white-space: nowrap">{scanNote(g)}</span>
                {/if}
              </div>
            </div>
          {/each}
        </section>
        {/if}
      {/if}
    </main>

    {#if !asking}
      <PhoneImportFooter>
        <button class="primary" onclick={() => scan.add()} disabled={scan.adding || (!scan.done && scan.chosen.length === 0)}>{scan.addLabel}</button>
      </PhoneImportFooter>
    {/if}
  </div>

  {#if permissionError}
    <div class="status err" role="status">{permissionError}</div>
  {:else if asking && !checking}
    <PhoneImportPermission {ios} oncancel={() => nav.back()} onscan={() => (asking = false)} />
  {/if}
</PhoneImportPage>

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
  .base {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }
  .body {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 8px calc(var(--safe-right) + 20px) calc(var(--safe-bottom) + 86px) calc(var(--safe-left) + 20px);
    display: flex;
    flex-direction: column;
    gap: 28px;
    scrollbar-width: none;
  }
  .body::-webkit-scrollbar {
    display: none;
  }
  h1 {
    margin: 0;
    font-size: 26px;
    line-height: 1.3;
    font-weight: 600;
  }
  .lbl {
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .seg {
    display: flex;
    padding: 2px;
    border-radius: 10px;
    background: var(--field);
    gap: 2px;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    flex: 1;
    height: 40px;
    display: grid;
    place-items: center;
    border-radius: 8px;
    font-size: 15px;
    color: var(--fg2);
    cursor: pointer;
    font-variant-numeric: tabular-nums;
  }
  .seg button.on {
    background: var(--seg-on);
    color: var(--fg);
    font-weight: 500;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.1);
  }
  .watch {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 52px;
    border-bottom: 1px solid var(--line);
  }
  .link {
    font-size: 14px;
    padding: 10px 0;
    text-decoration: none;
  }
  .seg button:focus-visible,
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
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
    min-height: 44px;
    cursor: pointer;
  }
  .pick input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .box {
    flex: 0 0 auto;
    width: 20px;
    height: 20px;
    border-radius: 6px;
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
  .sub {
    display: flex;
    align-items: center;
    gap: 10px;
    padding-left: 34px;
  }
  .ell {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
