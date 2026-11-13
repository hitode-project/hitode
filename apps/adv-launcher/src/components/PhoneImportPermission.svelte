<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "./Icon.svelte";
  import { COPY_SIZE } from "../lib/fake-import";
  import { inTauri } from "../lib/bridge";
  import { jit } from "../lib/jit.svelte";
  import { prefs, savePrefs } from "../lib/prefs.svelte";
  import { prepareFolder, storageSize } from "../lib/storage";
  import { nav } from "../lib/nav.svelte";

  interface Props {
    oncancel: () => void;
    /** A folder was picked, and copied if it had to be: scan it. */
    onscan: () => void;
  }

  let { oncancel, onscan }: Props = $props();

  type Step = "ask" | "copy";

  let step = $state<Step>("ask");
  let busy = $state(false);
  let error = $state("");
  let copySize = $state(inTauri ? "…" : COPY_SIZE);
  let operation: AbortController | undefined;
  let sheet = $state<HTMLElement>();
  let showLiveContainerHint = $state(false);

  $effect(() => {
    if (jit.status?.liveContainer && !prefs.liveContainerImportHintSeen) {
      showLiveContainerHint = true;
      prefs.liveContainerImportHintSeen = true;
      savePrefs();
    }
  });

  $effect(() => () => { operation?.abort(); });

  function go(next: Step) {
    step = next;
    error = "";
  }

  async function selectFolder() {
    operation = new AbortController();
    const controller = operation;
    busy = true;
    error = "";
    let total = 0;
    try {
      if (await prepareFolder(false, controller.signal,
        (bytes) => { total = bytes; copySize = storageSize(bytes); go("copy"); },
        (progress) => { copySize = `${storageSize(total)} · ${total ? Math.min(100, Math.floor(progress.bytes * 100 / total)) : 100}%`; },
      )) onscan();
    } catch (cause) {
      if (!controller.signal.aborted) error = String(cause);
    } finally {
      busy = false;
      operation = undefined;
    }
  }

  function cancel() {
    operation?.abort();
    oncancel();
  }

  function back() {
    operation?.abort();
    go("ask");
  }

  // Move focus into the dialog when it opens and on every step.
  $effect(() => {
    void step;
    sheet?.focus();
  });

  // The back gesture leaves the copy step for the first one; otherwise the screen closes.
  $effect(() =>
    nav.onBack(() => {
      if (step !== "copy") return false;
      back();
      return true;
    }),
  );

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") cancel();
  }
</script>

<svelte:window {onkeydown} />

<div class="layer">
  <button class="scrim" onclick={cancel} aria-label={t("关闭")}></button>
  <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="perm-t" tabindex="-1" bind:this={sheet}>
    <span class="grab" aria-hidden="true"></span>

    {#if step === "copy"}
      <div class="step">
        <div class="badge" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round"><rect x="6" y="2.5" width="12" height="19" rx="2.5"></rect><path d="M12 7.5v7M9 11.5l3 3 3-3"></path></svg>
        </div>
        <h2 id="perm-t" class="ttl">{t("复制到本机")}</h2>
        <p class="line">{error || `${t("副本预计额外占用")} ${copySize}`}</p>
        <div class="acts">
          <button class="primary" onclick={selectFolder} disabled={busy}>
            <Icon name="folder" size={17} stroke={1.5} />
            <span>{t("选择文件夹…")}</span>
          </button>
          <button class="quiet" onclick={back}>{t("返回")}</button>
        </div>
      </div>
    {:else}
      <div class="step">
        <div class="badge" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round"><path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4.2l2 2h8.8A1.5 1.5 0 0 1 21 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z"></path></svg>
        </div>
        <h2 id="perm-t" class="ttl">{t("选择游戏文件夹")}</h2>
        <p class="line">{error || t("在“文件”里选中整个游戏文件夹。")}</p>
        {#if showLiveContainerHint}
          <p class="line">{t("请先返回 LiveContainer，长按 Hitode → 设置，打开“使用 LiveContainer 的包名”，再重新启动 Hitode，否则可能在“文件”里点“打开”后没有反应。")}</p>
        {/if}
        <div class="acts">
          <button class="primary" onclick={selectFolder} disabled={busy}>{t("打开“文件”")}</button>
          <button class="quiet" onclick={oncancel}>{t("取消")}</button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .layer {
    --tint: rgba(55, 103, 214, 0.12);
    --badge: var(--accent);
    --grab: rgba(24, 24, 22, 0.16);
    --scrim: rgba(0, 0, 0, 0.28);
    --sheet: #ffffff;
    position: absolute;
    inset: 0;
    z-index: 5;
  }
  :global(:root[data-theme="dark"]) .layer {
    --tint: rgba(91, 134, 232, 0.16);
    --grab: rgba(255, 255, 255, 0.2);
    --scrim: rgba(0, 0, 0, 0.5);
    --sheet: #1e1e20;
  }
  :global(:root[data-theme="dark"][data-accent="blue"]) .layer {
    --badge: #7da0f0;
  }
  .scrim {
    all: unset;
    position: absolute;
    inset: 0;
    background: var(--scrim);
    cursor: pointer;
    animation: fade 0.2s ease;
  }
  .sheet {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    box-sizing: border-box;
    padding: 10px calc(var(--safe-right) + 24px) max(20px, var(--safe-bottom)) calc(var(--safe-left) + 24px);
    border-radius: 24px 24px 0 0;
    background: var(--sheet);
    box-shadow: 0 -10px 40px rgba(0, 0, 0, 0.25);
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    outline: none;
    animation: up 0.3s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  .grab {
    width: 36px;
    height: 5px;
    border-radius: 3px;
    background: var(--grab);
    margin-bottom: 22px;
  }
  .step {
    display: flex;
    flex-direction: column;
    align-items: center;
    align-self: stretch;
    animation: fade 0.22s ease;
  }
  .badge {
    width: 56px;
    height: 56px;
    border-radius: 28px;
    display: grid;
    place-items: center;
    background: var(--tint);
    color: var(--badge);
  }
  .ttl {
    margin: 16px 0 0;
    font-size: 20px;
    line-height: 1.35;
    font-weight: 600;
  }
  .line {
    margin: 8px 0 0;
    font-size: 14px;
    line-height: 1.55;
    color: var(--fg2);
    text-wrap: pretty;
  }
  .acts {
    align-self: stretch;
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 28px;
  }
  .acts .primary {
    height: 48px;
    padding: 0 28px;
    border-radius: 24px;
    font-size: 15px;
    font-weight: 500;
  }
  .acts .quiet {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: center;
    height: 44px;
    padding: 0 16px;
    border-radius: 22px;
    font-size: 14px;
  }
  .acts .quiet:focus-visible {
    outline-offset: 2px;
  }
  /* Beside the sidebar of a wide window, the sheet becomes a centered dialog
     whose scrim covers the whole window, not just the page's column. */
  :global(.column) .layer {
    position: fixed;
    z-index: 50;
    display: grid;
    place-items: center;
  }
  :global(.column) .sheet {
    position: relative;
    left: auto;
    right: auto;
    bottom: auto;
    width: min(440px, calc(100% - 48px));
    padding: 32px 28px 20px;
    border-radius: 24px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.22);
    animation: rise 0.24s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  :global(.column) .grab {
    display: none;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: scale(0.96);
    }
  }
  @keyframes up {
    from {
      transform: translateY(100%);
    }
    to {
      transform: none;
    }
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
    .sheet,
    .scrim,
    .step {
      animation: none;
    }
  }
</style>
