<script lang="ts">
  import ReportBackdrop from "../components/ReportBackdrop.svelte";
  import ReportResult from "../components/ReportResult.svelte";
  import { inTauri } from "../lib/bridge";
  import { pauser, runs } from "../lib/launch.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { PERF_ISSUES } from "../lib/report";
  import { ISSUE_LABELS, ReportForm, SITUATIONS } from "../lib/report-form.svelte";

  let { gameId }: { gameId: string } = $props();

  const game = $derived(library.get(gameId));
  const form = new ReportForm(() => game, () => runs.summaryOf(gameId), false);
  const ctx = $derived(form.context);

  // A report opened during play (from the pause menu) keeps the game paused.
  $effect(() => {
    if (runs.running === null) return;
    return pauser.hold();
  });
  const sending = $derived(form.sending);

  function close() {
    if (!sending) nav.back();
  }

  // Move focus into each result dialog as it appears.
  let stage = $state<HTMLElement>();
  $effect(() => {
    if (form.view === "form" || form.view === "sending") return;
    stage?.querySelector<HTMLElement>("[data-autofocus]")?.focus();
  });
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key !== "Escape") return;
    if (form.view === "error") form.view = "form";
    else close();
  }}
/>

<div class="report">
  <ReportBackdrop cover={game?.cover ?? "cv1"} dim />

  <div class="stage" bind:this={stage}>
    {#if game && form.loadError}
      <ReportResult id="rp-load" title="读不到游戏程序" onclose={() => nav.back()}>
        {#snippet badge()}
          <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><circle cx="8" cy="8" r="6"></circle><path d="M8 7.2v4M8 4.9h.01"></path></svg>
        {/snippet}
        <div class="path mono">{form.loadError}</div>
        <div class="acts"><button class="primary big" onclick={() => nav.back()}>返回</button></div>
      </ReportResult>
    {:else if game && !ctx}
      <!-- The host is identifying the main program. -->
    {:else if !game || !ctx}
      <ReportResult id="rp-none" title="找不到这个游戏" onclose={() => nav.home()}>
        {#snippet badge()}
          <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"><circle cx="8" cy="8" r="6"></circle><path d="M8 7.2v4M8 4.9h.01"></path></svg>
        {/snippet}
        <div class="acts"><button class="primary big" onclick={() => nav.home()}>返回</button></div>
      </ReportResult>
    {:else if form.view === "form" || form.view === "sending"}
      <div class="dlg" role="dialog" aria-modal="true" aria-labelledby="rp-title" aria-busy={sending}>
        <header class="head">
          <div class="who">
            <h2 id="rp-title">报告问题</h2>
            <div class="game">
              <div class="th" aria-hidden="true">
                <div class="art {game.cover}" class:blur={game.nsfw && prefs.blurAdult && !library.revealed[game.id]}></div>
              </div>
              <div class="names">
                <span lang="ja" class="title">{game.title}</span>
                <span class="mono build">{ctx.build}</span>
              </div>
            </div>
          </div>
          <button class="x" aria-label="关闭" onclick={close}>
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"></path></svg>
          </button>
        </header>

        <div class="body scroll" class:busy={sending}>
          <div>
            <p class="lbl" id="rp-sit">情况</p>
            <div class="tiles" role="radiogroup" aria-labelledby="rp-sit">
              {#each SITUATIONS as [key, label] (key)}
                <label class="tile" class:on={form.situation === key}>
                  <input type="radio" name="sit" value={key} bind:group={form.situation} />
                  <span>{label}</span>
                </label>
              {/each}
            </div>
            {#if form.isPerf}
              <div class="chips fade" role="group" aria-label="性能问题">
                {#each PERF_ISSUES as key (key)}
                  <button class="chip" class:on={form.issues[key]} aria-pressed={form.issues[key] ?? false} onclick={() => form.toggleIssue(key)}>
                    {#if form.issues[key]}
                      <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 8.5l3.2 3L13 4.5"></path></svg>
                    {/if}
                    <span>{ISSUE_LABELS[key]}</span>
                  </button>
                {/each}
              </div>
            {/if}
          </div>

          <div>
            <textarea class="note" rows="2" aria-label="补充说明（可选）" placeholder="补充说明（可选）" bind:value={form.note}></textarea>
            <div class="ttl">30 天后删除</div>
          </div>

          <div>
            <p class="lbl">将发送的内容</p>
            <div class="list">
              {#each form.rows as r (r.id)}
                <div class="rw" class:off={r.removed}>
                  <span class="k">{r.label}</span>
                  <span class="v" class:mono={!r.txt} class:txt={r.txt} title={r.value}>{r.value}</span>
                  {#if r.locked}
                    <span class="lock" role="img" aria-label="{r.label}：必需，无法移除">
                      <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true"><rect x="3.5" y="7" width="9" height="6.5" rx="1.5"></rect><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"></path></svg>
                    </span>
                  {:else if r.optional}
                    {@const key = r.optional}
                    {#if r.removed}
                      <button class="addback" aria-label="加回{r.label}" onclick={() => (form.off[key] = false)}>加回</button>
                    {:else}
                      <button class="act" aria-label="不发送{r.label}" onclick={() => (form.off[key] = true)}>
                        <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"></path></svg>
                      </button>
                    {/if}
                  {/if}
                </div>
              {/each}
            </div>
            <div class="foot-note">
              <span>不含文件路径、用户名、截图和存档</span>
              <button class="rawbtn" class:open={form.rawOpen} aria-expanded={form.rawOpen} aria-controls="rp-raw" onclick={() => (form.rawOpen = !form.rawOpen)}>
                <span>查看原始数据</span>
                <svg class="chev" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4"></path></svg>
              </button>
            </div>
            {#if form.rawOpen}
              <pre id="rp-raw" class="raw mono fade scroll" aria-label="原始数据">{form.raw}</pre>
            {/if}
          </div>
        </div>

        <footer class="foot">
          <button class={inTauri ? "primary big" : "secondary big"} onclick={() => form.save()} disabled={sending}>保存为文件</button>
          <!-- There is no report service yet: inside the app a report is saved as a file. -->
          {#if !inTauri}
            <button class="primary big" onclick={() => form.submit()} disabled={sending} style="min-width: 116px">
              {#if sending}
                <span class="spin" aria-hidden="true"></span><span>正在提交</span>
              {:else}
                <span>匿名提交</span>
              {/if}
            </button>
          {/if}
        </footer>
      </div>
    {:else if form.view === "sent"}
      <ReportResult id="rp-sent" title="已提交" ok>
        {#snippet badge()}
          <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5l3.2 3L13 4.5"></path></svg>
        {/snippet}
        <p class="sub">可在 设置 › 反馈与隐私 中撤回</p>
        <div class="acts"><button class="primary big" data-autofocus onclick={() => nav.back()} style="min-width: 116px">完成</button></div>
      </ReportResult>
    {:else if form.view === "error"}
      <ReportResult id="rp-err" title="没有连上服务器" onclose={() => (form.view = "form")}>
        {#snippet badge()}
          <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 6.5a8 8 0 0 1 11 0M4.6 8.8a5 5 0 0 1 6.8 0M7 11.2a1.6 1.6 0 0 1 2 0"></path><path d="M2.5 2.5l11 11"></path></svg>
        {/snippet}
        <div class="acts">
          <button class="secondary big" onclick={() => form.save()}>保存为文件</button>
          <button class="primary big" data-autofocus onclick={() => form.submit()} style="min-width: 96px">重试</button>
        </div>
      </ReportResult>
    {:else}
      <ReportResult id="rp-saved" title="已保存" ok>
        {#snippet badge()}
          <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.5v7.5M4.8 7l3.2 3.2L11.2 7"></path><path d="M3 12.5h10"></path></svg>
        {/snippet}
        <div class="path mono">{form.savedPath}</div>
        <div class="acts">
          <button class="primary big" data-autofocus onclick={() => nav.back()} style="min-width: 96px">完成</button>
        </div>
      </ReportResult>
    {/if}
  </div>
</div>

<style>
  .report {
    --surface: #fbfbfa;
    --tint: color-mix(in srgb, var(--accent) 8%, transparent);
    position: relative;
    height: 100%;
    overflow: clip;
    background: var(--bg);
    color: var(--fg);
  }
  :global(:root[data-theme="dark"]) .report {
    --surface: #1c1c1e;
    --tint: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .stage {
    position: relative;
    height: 100%;
    box-sizing: border-box;
    padding: 32px 24px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .dlg {
    position: relative;
    width: 680px;
    max-width: 100%;
    max-height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border-radius: 16px;
    box-shadow: 0 30px 80px rgba(0, 0, 0, 0.22), 0 0 0 1px var(--line);
    overflow: hidden;
  }
  .head {
    flex: 0 0 auto;
    display: flex;
    align-items: flex-start;
    gap: 14px;
    padding: 22px 20px 18px 28px;
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  h2 {
    margin: 0;
    font-size: 20px;
    line-height: 1.3;
    font-weight: 600;
  }
  .game {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  .th {
    position: relative;
    flex: 0 0 auto;
    width: 30px;
    aspect-ratio: 5 / 7;
    border-radius: 4px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--line);
  }
  .art {
    position: absolute;
    inset: 0;
  }
  .art.blur {
    filter: blur(6px) saturate(0.8);
    transform: scale(1.25);
  }
  .names {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .title {
    font-size: 14px;
    font-weight: 500;
  }
  .build {
    font-size: 11px;
  }
  .x {
    all: unset;
    box-sizing: border-box;
    width: 36px;
    height: 36px;
    display: grid;
    place-items: center;
    border-radius: 18px;
    color: var(--fg2);
    cursor: pointer;
  }
  .x:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    padding: 4px 28px 24px;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }
  .busy {
    opacity: 0.55;
    pointer-events: none;
  }
  .lbl {
    margin: 0 0 10px;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 8px;
  }
  .tile {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    text-align: center;
    min-height: 44px;
    padding: 0 10px;
    border-radius: 10px;
    background: var(--field);
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
    box-sizing: border-box;
    transition: background 0.12s ease;
  }
  .tile:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .tile.on {
    background: var(--tint);
    color: var(--fg);
    font-weight: 500;
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .tile input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .tile:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 12px;
  }
  .chip {
    all: unset;
    box-sizing: border-box;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0 14px;
    border-radius: 16px;
    background: var(--field);
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .chip:hover {
    color: var(--fg);
  }
  .chip.on {
    background: var(--tint);
    color: var(--fg);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .note {
    display: block;
    width: 100%;
    box-sizing: border-box;
    min-height: 64px;
    resize: none;
    border: 0;
    outline: 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--field);
    font-size: 13px;
    line-height: 1.55;
    color: var(--fg);
  }
  .note::placeholder {
    color: var(--fg3);
  }
  .note:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .ttl {
    margin-top: 6px;
    text-align: right;
    font-size: 11px;
    color: var(--fg3);
  }
  .list {
    border-top: 1px solid var(--line);
  }
  .rw {
    display: grid;
    grid-template-columns: 72px minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    min-height: 38px;
    border-bottom: 1px solid var(--line);
  }
  .rw .k {
    font-size: 13px;
    color: var(--fg2);
  }
  .rw .v {
    font-size: 12px;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    word-break: normal;
  }
  .rw .v.txt {
    font-size: 13px;
  }
  .rw.off .k {
    color: var(--fg3);
  }
  .rw.off .v {
    color: var(--fg3);
    text-decoration: line-through;
  }
  .act {
    all: unset;
    box-sizing: border-box;
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border-radius: 14px;
    color: var(--fg3);
    cursor: pointer;
  }
  .act:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .lock {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    color: var(--fg3);
  }
  .addback {
    all: unset;
    box-sizing: border-box;
    height: 28px;
    padding: 0 10px;
    display: flex;
    align-items: center;
    border-radius: 14px;
    font-size: 12px;
    color: var(--accent);
    cursor: pointer;
    white-space: nowrap;
  }
  .addback:hover {
    background: var(--hover);
  }
  .foot-note {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 10px;
  }
  .foot-note > span {
    flex: 1;
    font-size: 12px;
    color: var(--fg3);
  }
  .rawbtn {
    all: unset;
    box-sizing: border-box;
    height: 32px;
    padding: 0 12px;
    margin-right: -12px;
    display: flex;
    align-items: center;
    gap: 6px;
    border-radius: 16px;
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .rawbtn:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .rawbtn .chev {
    transition: transform 0.15s ease;
  }
  .rawbtn.open .chev {
    transform: rotate(180deg);
  }
  .raw {
    margin: 10px 0 0;
    padding: 12px 14px;
    border-radius: 10px;
    background: var(--field);
    font-size: 11px;
    line-height: 1.6;
    color: var(--fg2);
    white-space: pre;
    word-break: normal;
    overflow: auto;
    max-height: 180px;
    user-select: text;
    -webkit-user-select: text;
  }
  .foot {
    flex: 0 0 auto;
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    padding: 16px 28px 20px;
    border-top: 1px solid var(--line);
  }
  /* The report board's pill buttons are 40px tall. */
  .big {
    height: 40px;
    border-radius: 20px;
    white-space: nowrap;
  }
  .primary.big {
    padding: 0 22px;
  }
  .secondary.big {
    padding: 0 20px;
  }
  .primary.big:disabled {
    background: var(--accent);
    color: var(--on-accent);
  }
  .secondary.big:disabled {
    cursor: default;
    color: var(--fg3);
    background: var(--field);
  }
  .spin {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid color-mix(in srgb, var(--on-accent) 35%, transparent);
    border-top-color: var(--on-accent);
    animation: spin 0.8s linear infinite;
    box-sizing: border-box;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .sub {
    margin: 8px 0 0;
    font-size: 13px;
    color: var(--fg2);
  }
  .acts {
    display: flex;
    justify-content: center;
    gap: 10px;
    margin-top: 28px;
  }
  .path {
    margin-top: 12px;
    padding: 6px 12px;
    border-radius: 8px;
    background: var(--field);
    font-size: 12px;
    color: var(--fg2);
  }
  .reveal {
    all: unset;
    box-sizing: border-box;
    height: 40px;
    padding: 0 16px;
    display: flex;
    align-items: center;
    border-radius: 20px;
    font-size: 14px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .reveal:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .x:focus-visible,
  .chip:focus-visible,
  .act:focus-visible,
  .addback:focus-visible,
  .rawbtn:focus-visible,
  .reveal:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .fade {
    animation: fade 0.22s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fade,
    .spin {
      animation: none;
    }
    .tile,
    .rawbtn .chev {
      transition: none;
    }
  }
</style>
