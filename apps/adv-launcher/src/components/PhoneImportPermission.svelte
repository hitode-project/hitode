<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "./Icon.svelte";
  import { COPY_SIZE } from "../lib/fake-import";
  import { inTauri } from "../lib/bridge";
  import { prepareFolder, requestStoragePermission, storageSize } from "../lib/storage";
  import { nav } from "../lib/nav.svelte";

  interface Props {
    /** iOS picks folders through Files and needs no permission step. */
    ios: boolean;
    oncancel: () => void;
    /** A folder was picked, and copied if it had to be: scan it. */
    onscan: () => void;
  }

  let { ios, oncancel, onscan }: Props = $props();

  type Step = "ask" | "granted" | "copy";

  let step = $state<Step>("ask");
  /** The granted step's buttons appear once the system screen has settled. */
  let ready = $state(false);
  let busy = $state(false);
  let error = $state("");
  let copySize = $state(inTauri ? "…" : COPY_SIZE);
  let operation: AbortController | undefined;
  let sheet = $state<HTMLElement>();

  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => () => { clearTimeout(timer); operation?.abort(); });

  function go(next: Step) {
    clearTimeout(timer);
    step = next;
    ready = false;
    error = "";
  }

  async function grant() {
    busy = true;
    error = "";
    try {
      if (await requestStoragePermission()) {
        go("granted");
        timer = setTimeout(() => (ready = true), 900);
      }
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  async function selectFolder(copy: boolean) {
    operation = new AbortController();
    const controller = operation;
    busy = true;
    error = "";
    let total = 0;
    try {
      if (await prepareFolder(copy, controller.signal,
        (bytes) => { total = bytes; copySize = storageSize(bytes); if (!copy) go("copy"); },
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

    {#if !ios && step === "ask"}
      <div class="step">
        <div class="badge" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round"><path d="M11.5 19H4.5A1.5 1.5 0 0 1 3 17.5v-11A1.5 1.5 0 0 1 4.5 5h4.2l2 2h8.8A1.5 1.5 0 0 1 21 8.5V11"></path><path d="M18 13.2l3.5 1.3v2.7c0 2.2-1.5 3.8-3.5 4.6-2-.8-3.5-2.4-3.5-4.6v-2.7z"></path></svg>
        </div>
        <h2 id="perm-t" class="ttl">{t("允许访问游戏文件夹")}</h2>
        <p class="line">{error || t("游戏留在原处，启动器只读取，不会修改或上传。")}</p>
        <div class="acts">
          <button class="primary" onclick={grant} disabled={busy}>
            <span>{t("去授权")}</span>
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 4h6v6M12 4l-7.5 7.5"></path></svg>
          </button>
          <button class="secondary" onclick={() => go("copy")} disabled={busy}>{t("改为复制到本机")}</button>
          <button class="quiet" onclick={cancel}>{t("取消")}</button>
        </div>
      </div>
    {:else if !ios && step === "granted"}
      <div class="step">
        <div class="badge ok" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6.5 12.5l3.6 3.6L17.5 8.5"></path></svg>
        </div>
        <h2 id="perm-t" class="ttl">{error || t("已授权")}</h2>
        <div class="acts" class:wait={!ready}>
          <button class="primary" disabled={!ready || busy} onclick={() => selectFolder(false)}>
            <Icon name="folder" size={17} stroke={1.5} />
            <span>{t("选择文件夹…")}</span>
          </button>
          <button class="quiet" onclick={cancel} disabled={!ready}>{t("取消")}</button>
        </div>
      </div>
    {:else if step === "copy"}
      <div class="step">
        <div class="badge" aria-hidden="true">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round"><rect x="6" y="2.5" width="12" height="19" rx="2.5"></rect><path d="M12 7.5v7M9 11.5l3 3 3-3"></path></svg>
        </div>
        <h2 id="perm-t" class="ttl">{t("复制到本机")}</h2>
        <p class="line">{error || `${t("复制后可离线游玩，占用")} ${copySize}`}</p>
        <div class="acts">
          <button class="primary" onclick={() => selectFolder(true)} disabled={busy}>
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
        <div class="acts">
          <button class="primary" onclick={() => selectFolder(false)} disabled={busy}>{t("打开“文件”")}</button>
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
  .badge.ok {
    background: var(--accent);
    color: var(--on-accent);
    animation: pop 0.32s cubic-bezier(0.2, 0.9, 0.3, 1.3);
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
    transition: opacity 0.2s ease;
  }
  .acts.wait {
    opacity: 0;
    pointer-events: none;
  }
  .acts .primary,
  .acts .secondary {
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
  @keyframes pop {
    from {
      transform: scale(0.6);
    }
    to {
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .sheet,
    .scrim,
    .step,
    .badge.ok {
      animation: none;
    }
    .acts {
      transition: none;
    }
  }
</style>
