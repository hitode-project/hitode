<script lang="ts">
  import { onMount, untrack } from "svelte";
  import Icon from "../components/Icon.svelte";
  import { call, onHostEvent } from "../lib/bridge";
  import { pauser, runs } from "../lib/launch.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { modal, onOverlayInput, region, type Rect } from "../lib/overlay";

  /**
   * The in-game overlay. The native game view is under this transparent page;
   * only elements attached with `region` or `modal` take input from it, every
   * other press reaches the game (see lib/overlay.ts).
   *
   * The pause menu opens from the pause button (desktop; in the title bar on
   * macOS), the edge grip and the back gesture (phone), and ⌘. in the macOS
   * menu. Esc is never taken
   * from the game: many games use it themselves, so it only closes this
   * overlay's own layers while they hold the keyboard.
   */
  let { gameId, gameView, phone }: { gameId: string; gameView: boolean; phone: boolean } = $props();

  const game = $derived(library.get(gameId));

  type View = "closed" | "menu" | "confirm";
  let view = $state<View>("closed");
  /** The menu was opened by the back gesture; shows the edge hint briefly. */
  let backHint = $state(false);
  let textBar = $state(false);
  let text = $state("");
  let textInput = $state<HTMLInputElement | null>(null);

  type Scale = "int" | "fit" | "stretch";
  type Filter = "sharp" | "smooth";
  let scale = $state<Scale>("fit");
  let filter = $state<Filter>("smooth");
  let volume = $state(70);
  let muted = $state(false);
  let fps = $state(false);

  const started = Date.now();
  let now = $state(Date.now());
  onMount(() => {
    const timer = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(timer);
  });
  const sessionMinutes = $derived(Math.max(0, Math.floor((now - started) / 60_000)));

  const scales: { value: Scale; label: string }[] = [
    { value: "int", label: "整数倍" },
    { value: "fit", label: "适应" },
    { value: "stretch", label: "拉伸" },
  ];
  const filters: { value: Filter; label: string }[] = [
    { value: "sharp", label: "锐利" },
    { value: "smooth", label: "平滑" },
  ];

  // Diagnostic outline of the published regions: open the game screen with ?regions.
  const showRegions = new URLSearchParams(location.search).has("regions");
  let outlines = $state<Rect[]>([]);
  let outlineModal = $state(false);
  onMount(() =>
    showRegions
      ? onOverlayInput((input) => {
          outlines = input.regions;
          outlineModal = input.modal;
        })
      : undefined,
  );

  function openMenu() {
    textBar = false;
    view = "menu";
  }

  /** ⌘. and the back gesture: open the menu, or step back out of it. */
  function togglePause(fromBack: boolean) {
    if (textBar) textBar = false;
    else if (view === "confirm") view = "menu";
    else if (view === "menu") view = "closed";
    else {
      openMenu();
      backHint = fromBack;
    }
  }

  $effect(() => {
    if (!backHint) return;
    const timer = setTimeout(() => (backHint = false), 900);
    return () => clearTimeout(timer);
  });

  /** macOS: the window's title bar holds the pause button (see chrome.rs). */
  const titleBar = $derived(gameView && !phone);

  onMount(() => {
    void call("set_pause_available", { available: true }, null).catch(() => {});
    if (titleBar) {
      const title = untrack(() => game?.title) ?? null;
      void call("set_title_bar", { title }, null).catch(() => {});
    }
    const stopPause = onHostEvent("pause", () => togglePause(false));
    const stopBack = nav.onBack(() => {
      togglePause(true);
      return true;
    });
    return () => {
      void call("set_pause_available", { available: false }, null).catch(() => {});
      if (titleBar) void call("set_title_bar", { title: null }, null).catch(() => {});
      stopPause();
      stopBack();
    };
  });

  function openTextBar() {
    view = "closed";
    textBar = true;
    queueMicrotask(() => textInput?.focus());
  }

  async function send() {
    const value = text;
    if (!value) return;
    text = "";
    textBar = false;
    // Delivered to the guest as IME characters (Runtime::post_text).
    try {
      await call("post_text", { text: value }, value.length);
    } catch (error) {
      console.error("post_text failed", error);
    }
  }

  /** Asked once, the game quits on its own terms; asked again, it is forced. */
  const stopping = $derived(runs.stopping[gameId] === true);

  /**
   * Asks the game to quit and stays here: game_finished leaves this screen.
   * Without a native game view (a browser preview) there is no game to close.
   */
  function quit() {
    if (gameView) void runs.stop(gameId);
    else nav.home();
  }

  // The game is paused while the pause menu is open; once a quit is
  // requested the host resumes it so it can answer.
  $effect(() => {
    if (!gameView || view === "closed" || stopping) return;
    return pauser.hold();
  });

  function onkeydown(event: KeyboardEvent) {
    if (event.key !== "Escape" || event.isComposing) return;
    if (textBar) textBar = false;
    else if (view === "confirm") view = "menu";
    else if (view === "menu") view = "closed";
  }

  function onTextKey(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.isComposing) {
      event.preventDefault();
      void send();
    }
  }
</script>

<svelte:window {onkeydown} />

<div class="game" class:phone>
  {#if !gameView}
    <!-- Stand-in for the native game view when none is attached (browser, Linux). -->
    <div class="scene f-{phone && scale === "int" ? "fit" : scale}" aria-hidden="true">
      <div class="say">
        <div class="who" lang="ja">葵</div>
        <div class="line" lang="ja">「夏休み、まだ半分も残ってるんだよ。……だから、もう少しだけ」</div>
      </div>
    </div>
  {/if}

  {#if fps}
    <div class="fps mono">60 FPS</div>
  {/if}

  {#if view !== "closed"}
    <div class="scrim fade" {@attach modal}>
      {#if view === "menu"}
        <div class="panel" class:pop={!phone} class:slide={phone} role="dialog" aria-modal="true" aria-labelledby="pz-title">
          <div class="head">
            <div class="title">
              <h2 id="pz-title" lang="ja">{game?.title ?? "游戏"}</h2>
              <div class="sub">本次 {sessionMinutes} 分钟</div>
            </div>
            <button class="primary" onclick={() => (view = "closed")}>继续</button>
          </div>

          <div class="middle">
          <div class="rows">
            <div class="prow">
              <span>画面</span>
              <div class="seg" role="group" aria-label="画面">
                {#each scales as o (o.value)}
                  <button class:on={scale === o.value} aria-pressed={scale === o.value} onclick={() => (scale = o.value)}>{o.label}</button>
                {/each}
              </div>
            </div>
            <div class="prow">
              <span>过滤</span>
              <div class="seg" role="group" aria-label="过滤">
                {#each filters as o (o.value)}
                  <button class:on={filter === o.value} aria-pressed={filter === o.value} onclick={() => (filter = o.value)}>{o.label}</button>
                {/each}
              </div>
            </div>
            <div class="prow">
              <span>音量</span>
              <div class="vol">
                <button class="ibtn" class:on={muted} aria-label="静音" aria-pressed={muted} onclick={() => (muted = !muted)}>
                  <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M2.5 6h2.2L8 3.2v9.6L4.7 10H2.5z" />
                    {#if muted}<path d="M10.5 6l3.5 4M14 6l-3.5 4" />{:else}<path d="M10.6 5.6a3.4 3.4 0 0 1 0 4.8M12.4 3.9a5.8 5.8 0 0 1 0 8.2" />{/if}
                  </svg>
                </button>
                <div class="slider" class:muted>
                  <div class="slider-in">
                    <div class="rail"><div class="rfill" style="width: {volume}%"></div></div>
                    <div class="thumb" style="left: {volume}%"></div>
                  </div>
                  <input type="range" min="0" max="100" step="1" bind:value={volume} oninput={() => (muted = false)} aria-label="音量" />
                </div>
              </div>
            </div>
            <div class="prow">
              <span>帧率显示</span>
              <button class="sw" class:on={fps} role="switch" aria-checked={fps} aria-label="帧率显示" onclick={() => (fps = !fps)}></button>
            </div>
          </div>
          <div class="sep"></div>
          <div class="group">
            <button class="qrow" onclick={openTextBar}>
              <span>输入文字…</span>
              <span class="end"><Icon name="keyboard" /></span>
            </button>
            <button class="qrow" onclick={() => nav.go({ name: "report", gameId })}>报告问题…</button>
            <button class="qrow">
              <span>游戏文件夹</span>
              <span class="end"><Icon name="chevron-right" /></span>
            </button>
          </div>
          </div>
          <div class="sep"></div>
          <div class="group" style="padding-bottom: 10px">
            <button class="qrow danger" onclick={() => (view = "confirm")}>退出游戏</button>
          </div>
        </div>
      {:else}
        <div class="panel confirm" class:pop={!phone} class:slide={phone} role="alertdialog" aria-modal="true" aria-labelledby="pz-quit" aria-describedby="pz-quit-d">
          <h2 id="pz-quit">退出游戏？</h2>
          <p id="pz-quit-d">未在游戏内保存的进度会丢失</p>
          {#if phone}
            <div style="flex: 1"></div>
            <div style="display: flex; flex-direction: column; gap: 8px">
              <button class="danger-btn" onclick={quit}>{stopping ? "强制结束" : "退出"}</button>
              <button class="secondary" onclick={() => (view = "menu")}>取消</button>
            </div>
          {:else}
            <div style="display: flex; gap: 8px; margin-top: 18px">
              <button class="secondary" style="flex: 1" onclick={() => (view = "menu")}>取消</button>
              <button class="danger-btn" style="flex: 1" onclick={quit}>{stopping ? "强制结束" : "退出"}</button>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {:else if phone}
    <button class="grip fade" aria-label="暂停菜单" onclick={openMenu} {@attach region}><span></span></button>
  {:else if !titleBar}
    <button class="float fade" aria-label="暂停菜单" onclick={openMenu} {@attach region}>
      <Icon name="pause" />
    </button>
  {/if}

  {#if backHint}
    <div class="back" aria-hidden="true"><Icon name="chevron-left" size={18} stroke={1.8} /></div>
  {/if}

  {#if textBar}
    <div class="surface textbar fade" role="dialog" aria-label="输入文字" {@attach region}>
      <span class="kbd"><Icon name="keyboard" /></span>
      <input
        bind:this={textInput}
        bind:value={text}
        onkeydown={onTextKey}
        class="text"
        lang="ja"
        aria-label="要发送的文字"
        autocomplete="off"
        spellcheck="false"
      />
      <button class="quiet" style="height: 38px; box-sizing: border-box" onclick={() => (textBar = false)}>取消</button>
      <button class="primary" style="height: 38px" disabled={!text} onclick={send}>发送</button>
    </div>
  {/if}

  {#if showRegions}
    <div class="outlines" class:modal={outlineModal} aria-hidden="true">
      {#each outlines as r, i (i)}
        <div style="left: {r.x}px; top: {r.y}px; width: {r.width}px; height: {r.height}px"></div>
      {/each}
    </div>
  {/if}
</div>

<style>
  /* The overlay always uses the dark palette over the game. */
  .game {
    --panel: #1e1e20;
    --line: rgba(255, 255, 255, 0.08);
    --fg: #ececea;
    --fg2: #a9a9a5;
    --fg3: #8f8f8b;
    --field: rgba(255, 255, 255, 0.07);
    --hover: rgba(255, 255, 255, 0.05);
    --seg-on: #38383c;
    --rail: rgba(255, 255, 255, 0.14);
    --off: rgba(255, 255, 255, 0.2);
    --knob: #ececea;
    --accent: #3767d6;
    --on-accent: #ffffff;
    --danger: #ff8b7e;
    --danger-fill: #c8392e;
    color-scheme: dark;
    position: relative;
    height: 100%;
    overflow: hidden;
    display: grid;
    place-items: center;
    color: var(--fg);
  }
  .scene {
    position: relative;
    overflow: hidden;
    container-type: inline-size;
    background: radial-gradient(70% 50% at 25% 18%, #ffe7b8 0%, rgba(255, 231, 184, 0) 70%),
      radial-gradient(26% 52% at 40% 76%, rgba(255, 255, 255, 0.28) 0%, rgba(255, 255, 255, 0) 70%),
      radial-gradient(80% 60% at 85% 85%, #1f6fa8 0%, rgba(31, 111, 168, 0) 70%),
      linear-gradient(165deg, #a8dbf2 0%, #5ba9d6 55%, #2c6e9e 100%);
  }
  .game:has(.scene) {
    background: #000;
  }
  .f-fit {
    width: min(100%, 133.334vh);
    aspect-ratio: 4 / 3;
  }
  .f-int {
    width: 640px;
    height: 480px;
  }
  .f-stretch {
    width: 100%;
    height: 100%;
  }
  .say {
    position: absolute;
    left: 3cqw;
    right: 3cqw;
    bottom: 2.6cqw;
    padding: 1.6cqw 2.4cqw 2cqw;
    border-radius: 0.8cqw;
    background: rgba(10, 26, 46, 0.58);
    border: 1px solid rgba(255, 255, 255, 0.2);
  }
  .say .who {
    font-size: max(11px, 1.5cqw);
    color: #ffe1b0;
    margin-bottom: 0.6cqw;
  }
  .say .line {
    font-size: max(13px, 2.2cqw);
    line-height: 1.6;
    color: #fff;
  }
  .fps {
    position: absolute;
    top: 12px;
    left: 12px;
    padding: 3px 8px;
    border-radius: 6px;
    background: rgba(0, 0, 0, 0.6);
    color: #fff;
    font-variant-numeric: tabular-nums;
  }
  /* A transparent webview cannot blur the native view below, so the menu dims it instead. */
  .scrim {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.55);
  }
  .panel {
    width: 320px;
    box-sizing: border-box;
    border-radius: 16px;
    background: var(--panel);
    color: var(--fg);
    border: 1px solid var(--line);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5), 0 2px 8px rgba(0, 0, 0, 0.3);
  }
  .panel h2 {
    margin: 0;
    font-size: 15px;
    line-height: 1.4;
    font-weight: 600;
  }
  .title {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .vol {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .head {
    padding: 20px 20px 12px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .sub {
    font-size: 12px;
    color: var(--fg3);
  }
  .rows {
    padding: 0 20px 8px;
    display: flex;
    flex-direction: column;
  }
  .group {
    padding: 6px 10px;
    display: flex;
    flex-direction: column;
  }
  .confirm {
    padding: 24px 20px 20px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .confirm h2 {
    font-size: 17px;
  }
  .confirm p {
    margin: 0;
    font-size: 13px;
    line-height: 1.6;
    color: var(--fg2);
  }
  .panel .primary,
  .panel .secondary,
  .danger-btn {
    height: 40px;
    border-radius: 20px;
  }
  .danger-btn {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: center;
    height: 40px;
    border-radius: 20px;
    background: var(--danger-fill);
    color: #fff;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
  }
  .danger-btn:hover {
    filter: brightness(1.08);
  }
  .prow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 44px;
    font-size: 13px;
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    border-radius: 9px;
    background: var(--field);
    gap: 2px;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 12.5px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .seg button:hover {
    color: var(--fg);
  }
  .seg button.on {
    background: var(--seg-on);
    color: var(--fg);
    font-weight: 500;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
  }
  .sw {
    all: unset;
    box-sizing: border-box;
    position: relative;
    flex: 0 0 auto;
    width: 40px;
    height: 24px;
    border-radius: 12px;
    background: var(--off);
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .sw::after {
    content: "";
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 9px;
    background: var(--knob);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.3);
    transition: transform 0.15s ease;
  }
  .sw.on {
    background: var(--accent);
  }
  .sw.on::after {
    transform: translateX(16px);
    background: #fff;
  }
  .ibtn {
    all: unset;
    box-sizing: border-box;
    flex: 0 0 auto;
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
    border-radius: 16px;
    color: var(--fg2);
    cursor: pointer;
  }
  .ibtn:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .ibtn.on {
    color: var(--danger);
  }
  .slider {
    position: relative;
    width: 132px;
    height: 28px;
    flex: 0 0 auto;
  }
  .slider-in {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 7px;
    right: 7px;
  }
  .rail {
    position: absolute;
    left: 0;
    right: 0;
    top: 50%;
    height: 4px;
    margin-top: -2px;
    border-radius: 2px;
    background: var(--rail);
    overflow: hidden;
  }
  .rfill {
    height: 100%;
    background: var(--fg);
  }
  .thumb {
    position: absolute;
    top: 50%;
    width: 14px;
    height: 14px;
    margin: -7px 0 0 -7px;
    border-radius: 7px;
    background: #fff;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.45);
  }
  .slider.muted .rfill,
  .slider.muted .thumb {
    background: var(--fg3);
  }
  .slider input {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    cursor: pointer;
  }
  .slider:has(input:focus-visible) .thumb {
    outline: 2px solid #8fb0f5;
    outline-offset: 2px;
  }
  .qrow {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 36px;
    padding: 0 10px;
    border-radius: 8px;
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
  }
  .qrow:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .qrow .end {
    margin-left: auto;
    color: var(--fg3);
    display: flex;
  }
  .qrow.danger {
    color: var(--danger);
  }
  .qrow.danger:hover {
    background: rgba(255, 139, 126, 0.1);
  }
  .sep {
    height: 1px;
    background: var(--line);
  }
  .float {
    all: unset;
    box-sizing: border-box;
    position: absolute;
    top: 16px;
    right: 16px;
    width: 40px;
    height: 40px;
    display: grid;
    place-items: center;
    border-radius: 20px;
    background: rgba(18, 18, 20, 0.58);
    backdrop-filter: blur(16px) saturate(1.2);
    border: 1px solid rgba(255, 255, 255, 0.1);
    color: rgba(255, 255, 255, 0.9);
    cursor: pointer;
  }
  .float:hover {
    background: rgba(40, 40, 44, 0.72);
  }
  .surface {
    background: var(--panel);
    color: var(--fg);
    border: 1px solid var(--line);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5), 0 2px 8px rgba(0, 0, 0, 0.3);
  }
  .textbar {
    position: absolute;
    left: 50%;
    bottom: 28px;
    transform: translateX(-50%);
    width: min(520px, calc(100% - 32px));
    border-radius: 999px;
    padding: 6px;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .kbd {
    flex: 0 0 auto;
    width: 36px;
    display: grid;
    place-items: center;
    color: var(--fg3);
  }
  .text {
    all: unset;
    flex: 1;
    min-width: 0;
    height: 38px;
    padding: 0 8px;
    font-size: 15px;
    color: var(--fg);
  }
  .primary:focus-visible,
  .secondary:focus-visible,
  .danger-btn:focus-visible,
  .seg button:focus-visible,
  .sw:focus-visible,
  .ibtn:focus-visible,
  .qrow:focus-visible,
  .float:focus-visible {
    outline: 2px solid #8fb0f5;
    outline-offset: 2px;
  }
  .pop {
    animation: pop 0.18s ease;
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.97);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  .fade {
    animation: fade 0.18s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  /* Phone (landscape): the menu is a sheet on the right edge, with larger targets. */
  .phone .scrim {
    place-items: stretch;
  }
  .phone .panel {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    width: calc(300px + var(--safe-right));
    padding-right: var(--safe-right);
    display: flex;
    flex-direction: column;
    border: none;
    border-radius: 16px 0 0 16px;
    box-shadow: -16px 0 48px rgba(0, 0, 0, 0.45);
  }
  .phone .head {
    padding: 14px 16px 8px;
    gap: 10px;
  }
  .phone .title {
    flex-direction: row;
    align-items: baseline;
    gap: 10px;
  }
  .phone .title h2 {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .phone .title .sub {
    flex: 0 0 auto;
  }
  .phone .vol {
    flex: 1 1 auto;
    min-width: 0;
    gap: 4px;
  }
  .phone .middle {
    flex: 1 1 0;
    min-height: 0;
    overflow-y: auto;
    scrollbar-width: none;
  }
  .phone .rows {
    padding: 0 16px 6px;
  }
  .phone .prow {
    min-height: 52px;
    font-size: 14px;
  }
  .phone .seg {
    border-radius: 11px;
  }
  .phone .seg button {
    height: 36px;
    padding: 0 12px;
    border-radius: 9px;
    font-size: 13px;
  }
  .phone .ibtn {
    width: 44px;
    height: 44px;
    border-radius: 22px;
  }
  .phone .slider {
    flex: 1 1 auto;
    width: auto;
    min-width: 0;
    height: 44px;
  }
  .phone .qrow {
    height: 48px;
    padding: 0 12px;
    border-radius: 10px;
    font-size: 14px;
  }
  .phone .panel .primary,
  .phone .panel .secondary,
  .phone .danger-btn {
    height: 48px;
    border-radius: 24px;
    font-size: 15px;
  }
  .phone .confirm {
    padding: 22px 16px 12px;
  }
  .phone .confirm h2 {
    font-size: 18px;
  }
  .phone .confirm p {
    margin-top: 6px;
    font-size: 14px;
  }
  .phone .fps {
    top: 10px;
    left: calc(10px + var(--safe-left));
    padding: 2px 7px;
    font-size: 11px;
  }
  .grip {
    all: unset;
    box-sizing: border-box;
    position: absolute;
    right: var(--safe-right);
    top: 50%;
    transform: translateY(-50%);
    width: 44px;
    height: 88px;
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  .grip span {
    display: block;
    width: 4px;
    height: 36px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.32);
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.2);
  }
  .grip:active span {
    background: rgba(255, 255, 255, 0.6);
  }
  .grip:focus-visible {
    outline: 2px solid #8fb0f5;
    outline-offset: 1px;
  }
  .back {
    position: absolute;
    left: 0;
    top: 50%;
    width: 34px;
    height: 76px;
    margin-top: -38px;
    border-radius: 0 38px 38px 0;
    background: rgba(236, 236, 234, 0.92);
    color: #141415;
    display: grid;
    place-items: center;
    padding-right: 4px;
    box-sizing: border-box;
    animation: back 0.5s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  @keyframes back {
    from {
      transform: translateX(-34px);
    }
    to {
      transform: none;
    }
  }
  .slide {
    animation: slide 0.2s ease;
  }
  @keyframes slide {
    from {
      opacity: 0;
      transform: translateX(32px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  .outlines {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .outlines.modal {
    box-shadow: inset 0 0 0 3px rgba(255, 64, 160, 0.8);
  }
  .outlines div {
    position: absolute;
    box-sizing: border-box;
    border: 2px dashed rgba(255, 64, 160, 0.9);
  }
  @media (prefers-reduced-motion: reduce) {
    .pop,
    .fade,
    .slide,
    .back {
      animation: none;
    }
    .sw,
    .sw::after {
      transition: none;
    }
  }
</style>
