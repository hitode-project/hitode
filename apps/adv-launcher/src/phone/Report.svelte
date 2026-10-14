<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import PhoneReportResult from "../components/PhoneReportResult.svelte";
  import { pauser, runs } from "../lib/launch.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { PERF_ISSUES } from "../lib/report";
  import { ISSUE_LABELS, ReportForm, SITUATIONS } from "../lib/report-form.svelte";

  let { gameId }: { gameId: string } = $props();

  const game = $derived(library.get(gameId));
  const form = new ReportForm(() => game, () => runs.summaryOf(gameId), true);
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

  // The back gesture leaves the error screen for the form and waits out a submit.
  $effect(() =>
    nav.onBack(() => {
      if (form.view === "error") {
        form.view = "form";
        return true;
      }
      return form.view === "sending";
    }),
  );

  // Move focus into each result screen as it appears.
  let root = $state<HTMLElement>();
  $effect(() => {
    if (form.view === "form" || form.view === "sending") return;
    root?.querySelector<HTMLElement>("[data-autofocus]")?.focus();
  });
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key !== "Escape") return;
    if (form.view === "error") form.view = "form";
    else close();
  }}
/>

<div class="report" bind:this={root}>
  {#if game && form.loadError}
    <PhoneReportResult id="pr-load" title={t("读不到游戏程序")} onclose={() => nav.back()}>
      {#snippet badge()}
        <svg width="26" height="26" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><circle cx="8" cy="8" r="6"></circle><path d="M8 7.2v4M8 4.9h.01"></path></svg>
      {/snippet}
      <div class="path mono">{form.loadError}</div>
      {#snippet actions()}
        <button class="primary big" onclick={() => nav.back()}>{t("返回")}</button>
      {/snippet}
    </PhoneReportResult>
  {:else if game && !ctx}
    <!-- The host is identifying the main program. -->
  {:else if !game || !ctx}
    <PhoneReportResult id="pr-none" title={t("找不到这个游戏")} onclose={() => nav.home()}>
      {#snippet badge()}
        <svg width="26" height="26" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"><circle cx="8" cy="8" r="6"></circle><path d="M8 7.2v4M8 4.9h.01"></path></svg>
      {/snippet}
      {#snippet actions()}
        <button class="primary big" onclick={() => nav.home()}>{t("返回")}</button>
      {/snippet}
    </PhoneReportResult>
  {:else if form.view === "form" || form.view === "sending"}
    <div class="dlg" role="dialog" aria-modal="true" aria-labelledby="pr-title" aria-busy={sending}>
      <header class="head">
        <button class="x" aria-label={t("关闭")} onclick={close}>
          <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"></path></svg>
        </button>
      </header>

      <main class="body" class:busy={sending}>
        <div class="who">
          <h1 id="pr-title">{t("报告问题")}</h1>
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

        <div>
          <p class="lbl" id="pr-sit">{t("情况")}</p>
          <div class="tiles" role="radiogroup" aria-labelledby="pr-sit">
            {#each SITUATIONS as [key, label] (key)}
              <label class="tile" class:on={form.situation === key}>
                <input type="radio" name="psit" value={key} bind:group={form.situation} />
                <span>{t(label)}</span>
              </label>
            {/each}
          </div>
          {#if form.isPerf}
            <div class="chips fade" role="group" aria-label={t("性能问题")}>
              {#each PERF_ISSUES as key (key)}
                <button class="chip" class:on={form.issues[key]} aria-pressed={form.issues[key] ?? false} onclick={() => form.toggleIssue(key)}>
                  {#if form.issues[key]}
                    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 8.5l3.2 3L13 4.5"></path></svg>
                  {/if}
                  <span>{t(ISSUE_LABELS[key])}</span>
                </button>
              {/each}
            </div>
          {/if}
        </div>

        <div>
          <textarea class="note" rows="3" aria-label={t("补充说明（可选）")} placeholder={t("补充说明（可选）")} bind:value={form.note}></textarea>
          <div class="ttl">{t("30 天后删除")}</div>
        </div>

        <div>
          <p class="lbl">{t("将发送的内容")}</p>
          <div class="list">
            {#each form.rows as r (r.id)}
              <div class="rw" class:off={r.removed}>
                <span class="k">{r.label}</span>
                <span class="v" class:mono={!r.txt} class:txt={r.txt}>{r.value}</span>
                {#if r.locked}
                  <span class="lock" role="img" aria-label={t("{0}：必需，无法移除", r.label)}>
                    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true"><rect x="3.5" y="7" width="9" height="6.5" rx="1.5"></rect><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"></path></svg>
                  </span>
                {:else if r.optional}
                  {@const key = r.optional}
                  {#if r.removed}
                    <button class="addback" aria-label={t("加回{0}", r.label)} onclick={() => (form.off[key] = false)}>{t("加回")}</button>
                  {:else}
                    <button class="act" aria-label={t("不发送{0}", r.label)} onclick={() => (form.off[key] = true)}>
                      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"></path></svg>
                    </button>
                  {/if}
                {/if}
              </div>
            {/each}
          </div>
          <div class="privacy">{t("不含文件路径、用户名、截图和存档")}</div>
          <button class="rawbtn" class:open={form.rawOpen} aria-expanded={form.rawOpen} aria-controls="pr-raw" onclick={() => (form.rawOpen = !form.rawOpen)}>
            <span>{t("查看原始数据")}</span>
            <svg class="chev" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4"></path></svg>
          </button>
          {#if form.rawOpen}
            <pre id="pr-raw" class="raw mono fade" aria-label={t("原始数据")}>{form.raw}</pre>
          {/if}
        </div>
      </main>

      <footer class="foot">
        <button class="primary big" onclick={() => form.save()} disabled={sending}>{t("保存为文件")}</button>

      </footer>
    </div>
  {:else if form.view === "sent"}
    <PhoneReportResult id="pr-sent" title={t("已提交")} ok>
      {#snippet badge()}
        <svg width="26" height="26" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5l3.2 3L13 4.5"></path></svg>
      {/snippet}
      <p class="sub">{t("可在 设置 › 反馈与隐私 中撤回")}</p>
      {#snippet actions()}
        <button class="primary big" data-autofocus onclick={() => nav.back()}>{t("完成")}</button>
      {/snippet}
    </PhoneReportResult>
  {:else if form.view === "error"}
    <PhoneReportResult id="pr-err" title={t("没有连上服务器")} onclose={() => (form.view = "form")}>
      {#snippet badge()}
        <svg width="26" height="26" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 6.5a8 8 0 0 1 11 0M4.6 8.8a5 5 0 0 1 6.8 0M7 11.2a1.6 1.6 0 0 1 2 0"></path><path d="M2.5 2.5l11 11"></path></svg>
      {/snippet}
      {#snippet actions()}
        <button class="primary big" data-autofocus onclick={() => form.submit()}>{t("重试")}</button>
        <button class="secondary big" onclick={() => form.save()}>{t("保存为文件")}</button>
      {/snippet}
    </PhoneReportResult>
  {:else}
    <PhoneReportResult id="pr-saved" title={t("已保存")} ok tight>
      {#snippet badge()}
        <svg width="26" height="26" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.5v7.5M4.8 7l3.2 3.2L11.2 7"></path><path d="M3 12.5h10"></path></svg>
      {/snippet}
      <div class="path mono">{form.savedPath}</div>
      {#snippet actions()}
        <button class="primary big" data-autofocus onclick={() => nav.back()}>{t("完成")}</button>
      {/snippet}
    </PhoneReportResult>
  {/if}
</div>

<style>
  .report {
    --tint: color-mix(in srgb, var(--accent) 8%, transparent);
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg);
    color: var(--fg);
  }
  :global(:root[data-theme="dark"]) .report {
    --tint: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .dlg {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .head {
    flex: 0 0 auto;
    padding: calc(var(--safe-top) + 6px) max(8px, var(--safe-right)) 0 max(8px, var(--safe-left));
  }
  .x {
    all: unset;
    box-sizing: border-box;
    width: 44px;
    height: 44px;
    display: grid;
    place-items: center;
    border-radius: 22px;
    color: var(--fg);
    cursor: pointer;
  }
  .body {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 6px max(20px, var(--safe-right)) 28px max(20px, var(--safe-left));
    display: flex;
    flex-direction: column;
    gap: 28px;
    scrollbar-width: none;
  }
  .body::-webkit-scrollbar {
    display: none;
  }
  .busy {
    opacity: 0.55;
    pointer-events: none;
  }
  .who {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  h1 {
    margin: 0;
    font-size: 28px;
    line-height: 1.25;
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
    width: 32px;
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
    font-size: 15px;
    font-weight: 500;
  }
  .build {
    font-size: 11px;
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
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }
  .tile {
    position: relative;
    display: flex;
    align-items: center;
    min-height: 56px;
    padding: 0 14px;
    border-radius: 12px;
    background: var(--field);
    font-size: 14px;
    line-height: 1.35;
    color: var(--fg2);
    cursor: pointer;
    box-sizing: border-box;
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
    height: 44px;
    padding: 0 16px;
    border-radius: 22px;
    background: var(--field);
    font-size: 14px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
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
    min-height: 84px;
    resize: none;
    border: 0;
    outline: 0;
    padding: 12px 14px;
    border-radius: 12px;
    background: var(--field);
    font-size: 15px;
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
    grid-template-columns: 64px minmax(0, 1fr) auto;
    align-items: center;
    gap: 10px;
    min-height: 48px;
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
    font-size: 14px;
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
    width: 44px;
    height: 44px;
    margin-right: -12px;
    display: grid;
    place-items: center;
    border-radius: 22px;
    color: var(--fg3);
    cursor: pointer;
  }
  .lock {
    width: 44px;
    height: 44px;
    margin-right: -12px;
    display: grid;
    place-items: center;
    color: var(--fg3);
  }
  .addback {
    all: unset;
    box-sizing: border-box;
    height: 44px;
    padding: 0 12px;
    margin-right: -12px;
    display: flex;
    align-items: center;
    border-radius: 22px;
    font-size: 14px;
    color: var(--accent);
    cursor: pointer;
    white-space: nowrap;
  }
  .privacy {
    margin-top: 10px;
    font-size: 12px;
    color: var(--fg3);
  }
  .rawbtn,
  .reveal {
    all: unset;
    box-sizing: border-box;
    height: 44px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border-radius: 22px;
    font-size: 14px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .rawbtn {
    margin: 4px 0 0 -12px;
  }
  .rawbtn .chev {
    transition: transform 0.15s ease;
  }
  .rawbtn.open .chev {
    transform: rotate(180deg);
  }
  .reveal {
    height: 48px;
    font-size: 15px;
  }
  .raw {
    margin: 8px 0 0;
    padding: 12px 14px;
    border-radius: 12px;
    background: var(--field);
    font-size: 11px;
    line-height: 1.6;
    color: var(--fg2);
    white-space: pre-wrap;
    word-break: break-all;
    overflow: auto;
    user-select: text;
    -webkit-user-select: text;
  }
  .foot {
    flex: 0 0 auto;
    display: flex;
    gap: 10px;
    padding: 12px max(20px, var(--safe-right)) max(var(--safe-bottom), 16px) max(20px, var(--safe-left));
    border-top: 1px solid var(--line);
    background: var(--bg);
  }
  .foot > button {
    flex: 1 1 0;
  }
  /* The phone report board's pill buttons are 48px tall. */
  .big {
    height: 48px;
    border-radius: 24px;
    font-size: 16px;
    white-space: nowrap;
  }
  .primary.big {
    padding: 0 20px;
  }
  .secondary.big {
    padding: 0 18px;
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
    width: 16px;
    height: 16px;
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
    font-size: 14px;
    color: var(--fg2);
  }
  .path {
    margin-top: 12px;
    padding: 6px 12px;
    border-radius: 8px;
    background: var(--field);
    font-size: 12px;
    color: var(--fg2);
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
    }
    to {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fade,
    .spin {
      animation: none;
    }
    .rawbtn .chev {
      transition: none;
    }
  }
</style>
