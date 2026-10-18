<script lang="ts">
  import { onDestroy } from "svelte";
  import { t } from "../lib/i18n.svelte";
  import { call } from "../lib/bridge";
  import { region } from "../lib/overlay";
  import { prefs, savePrefs } from "../lib/prefs.svelte";
  import type { GameLayout } from "../lib/screen.svelte";
  import { arrange, keyHeight } from "../lib/touch-layout";

  /**
   * Touch controls around a mobile game (AND-APP-16): the library and menu
   * buttons on one side, the keys a mouse-and-keyboard game needs on the
   * other. In trackpad mode the sides hold big left and right buttons
   * instead. The layout follows the letterbox the native view reports.
   */
  interface Props {
    layout: GameLayout;
    onlibrary: () => void;
    onmenu: () => void;
    onkeyboard: () => void;
    /** The game's Ctrl setting is 切换: a tap latches Ctrl instead of holding it. */
    latchCtrl: boolean;
  }
  let { layout, latchCtrl, onlibrary, onmenu, onkeyboard }: Props = $props();

  const arrangement = $derived(arrange(layout));
  const trackpad = $derived(prefs.touchMode === "trackpad");
  const open = $derived(prefs.touchControls);
  let more = $state(false);
  let ctrl = $state(false);

  function toggleOpen() {
    prefs.touchControls = !open;
    more = false;
    savePrefs();
  }
  function toggleTrackpad() {
    prefs.touchMode = trackpad ? "direct" : "trackpad";
    more = false;
    savePrefs();
  }
  function toggleHud() {
    prefs.showFps = !prefs.showFps;
    savePrefs();
  }

  type Action =
    | { kind: "mouse"; right: boolean }
    | { kind: "key"; key: number }
    | { kind: "ctrl" }
    | { kind: "wheel"; notches: number }
    | { kind: "keyboard" }
    | { kind: "trackpad" }
    | { kind: "more" };
  interface Key {
    label: string;
    action: Action;
  }
  const right: Key = { label: "右键", action: { kind: "mouse", right: true } };
  const enter: Key = { label: "Enter", action: { kind: "key", key: 0x0d } };
  const esc: Key = { label: "Esc", action: { kind: "key", key: 0x1b } };
  const space: Key = { label: "Space", action: { kind: "key", key: 0x20 } };
  const ctrlKey: Key = { label: "Ctrl", action: { kind: "ctrl" } };
  const tab: Key = { label: "Tab", action: { kind: "key", key: 0x09 } };
  const up: Key = { label: "滚轮 ↑", action: { kind: "wheel", notches: 1 } };
  const down: Key = { label: "滚轮 ↓", action: { kind: "wheel", notches: -1 } };
  const keyboard: Key = { label: "键盘", action: { kind: "keyboard" } };
  const pad: Key = { label: "触控板", action: { kind: "trackpad" } };
  const moreKey: Key = { label: "更多", action: { kind: "more" } };
  const full = [right, enter, esc, space, ctrlKey, tab, up, down, keyboard, pad];
  const compact = [right, enter, ctrlKey, keyboard, moreKey];
  const extra = [esc, space, tab, up, down, pad];

  // Buttons and keys go down with the finger and up when it lifts, so a
  // held key or button stays held.
  function press(action: Action, held: boolean) {
    switch (action.kind) {
      case "mouse":
        void call("game_mouse_button", { right: action.right, down: held }, null).catch(() => {});
        return;
      case "key":
        void call("game_key", { key: action.key, down: held }, null).catch(() => {});
        return;
      case "ctrl":
        if (latchCtrl && !held) return;
        ctrl = latchCtrl ? !ctrl : held;
        void call("game_key", { key: 0x11, down: ctrl }, null).catch(() => {});
        return;
    }
    if (!held) return;
    switch (action.kind) {
      case "wheel":
        void call("game_wheel", { notches: action.notches }, null).catch(() => {});
        return;
      case "keyboard":
        more = false;
        onkeyboard();
        return;
      case "trackpad":
        toggleTrackpad();
        return;
      case "more":
        more = !more;
    }
  }

  /** Pointer handlers for a key: pressed while the finger is down on it. */
  function hold(action: Action) {
    let down = false;
    return {
      onpointerdown(event: PointerEvent) {
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        down = true;
        press(action, true);
      },
      onpointerup() {
        if (down) press(action, false);
        down = false;
      },
      onpointercancel() {
        if (down) press(action, false);
        down = false;
      },
      oncontextmenu(event: Event) {
        event.preventDefault();
      },
    };
  }

  // A held Ctrl lifts when the controls go away; the pause menu releases
  // the game's keys itself.
  onDestroy(() => {
    if (ctrl) void call("game_key", { key: 0x11, down: false }, null).catch(() => {});
  });

  const bars = $derived(arrangement.kind === "side" ? arrangement : null);
  /** Two columns of keys fit a tablet's letterbox even without labels. */
  const two = $derived(!!bars && (bars.wide || bars.right.width >= 96));
  const keys = $derived(bars && !two ? compact : full);
  const keyH = $derived(
    bars ? keyHeight(Math.min(bars.left.height, bars.right.height), two ? 5 : keys.length, 50) : 44,
  );
  /** A tablet's tall letterbox: the groups sit where the thumbs rest, not at the top. */
  const tall = $derived(!!bars && Math.min(bars.left.height, bars.right.height) >= 560);
  const style = (a: { x: number; y: number; width: number; height: number }) =>
    `left: ${a.x}px; top: ${a.y}px; width: ${a.width}px; height: ${a.height}px`;
</script>

{#snippet icon(name: "library" | "menu" | "perf" | "hide" | "pad" | "keyboard")}
  <svg width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    {#if name === "library"}<path d="M9.5 3.5L5 8l4.5 4.5" />
    {:else if name === "menu"}<path d="M3 4.5h10M3 8h10M3 11.5h10" />
    {:else if name === "perf"}<path d="M2 9h2.5l1.5-4 3 7 1.5-4H14" />
    {:else if name === "hide"}<path d="M6.5 3.5L11 8l-4.5 4.5" />
    {:else if name === "pad"}<rect x="2" y="3" width="12" height="10" rx="2" /><path d="M2 10h12M8 10v3" />
    {:else}<rect x="1.5" y="4" width="13" height="8" rx="1.5" /><path d="M4 6.5h.01M6.5 6.5h.01M9 6.5h.01M11.5 6.5h.01M5 9.5h6" />
    {/if}
  </svg>
{/snippet}

{#snippet keyButton(key: Key, height: number)}
  <button class="key" class:hold={key.action.kind === "ctrl" && ctrl} class:on={key.action.kind === "more" && more}
    style="height: {height}px" {...hold(key.action)}>{t(key.label)}</button>
{/snippet}

{#snippet sideButtons(labels: boolean)}
  {#each [["library", "游戏库", onlibrary], ["menu", "菜单", onmenu], ["perf", "性能", toggleHud]] as const as [name, label, action] (name)}
    <button class={labels ? "side-btn" : "side-icon"} class:on={name === "perf" && prefs.showFps} aria-label={t(label)} onclick={action}>
      {@render icon(name)}
      {#if labels}<span>{t(label)}</span>{/if}
    </button>
  {/each}
{/snippet}

{#if bars}
  {#if open}
    <div class="column fade" class:tight={bars.left.width < 44} class:middle={tall && !trackpad} style={style(bars.left)}>
      <div class="group" class:labels={bars.wide && !trackpad} {@attach region}>
        {#if trackpad}
          <button class="side-icon" aria-label={t("游戏库")} onclick={onlibrary}>{@render icon("library")}</button>
          <button class="side-icon" aria-label={t("菜单")} onclick={onmenu}>{@render icon("menu")}</button>
        {:else}
          {@render sideButtons(bars.wide)}
        {/if}
      </div>
      {#if trackpad}
        <div class="spacer"></div>
        <button class="big" {@attach region} {...hold({ kind: "mouse", right: false })}>{t("左键")}</button>
      {/if}
    </div>
    <div class="column fade" class:tight={bars.right.width < 44} class:middle={tall && !trackpad} style={style(bars.right)}>
      <div class="group" {@attach region}>
        <button class="hide" aria-label={t("收起")} onclick={toggleOpen}>{@render icon("hide")}</button>
        {#if trackpad}
          <button class="side-icon on" aria-label={t("触控板模式")} aria-pressed="true" onclick={toggleTrackpad}>{@render icon("pad")}</button>
          <button class="side-icon" aria-label={t("键盘")} onclick={onkeyboard}>{@render icon("keyboard")}</button>
        {:else}
          <div class="keys" class:two>
            {#each keys as key (key.label)}{@render keyButton(key, keyH)}{/each}
          </div>
        {/if}
      </div>
      {#if trackpad}
        <div class="spacer"></div>
        <button class="big" {@attach region} {...hold({ kind: "mouse", right: true })}>{t("右键")}</button>
      {/if}
      {#if more && !trackpad}
        <div class="more fade" style="right: {bars.right.width + 8}px" {@attach region}>
          {#each extra as key (key.label)}{@render keyButton(key, 40)}{/each}
        </div>
      {/if}
    </div>
  {:else}
    <div class="column" style={style(bars.right)}>
      <div class="spacer"></div>
      <button class="grip" aria-label={t("展开")} onclick={toggleOpen} {@attach region}><span></span></button>
      <div class="spacer"></div>
    </div>
  {/if}
{:else if arrangement.kind === "band"}
  {#if open}
    <div class="row fade" style={style(arrangement.top)}>
      <div class="group across" {@attach region}>{@render sideButtons(true)}</div>
      <div class="spacer"></div>
      <button class="hide down" aria-label={t("收起")} onclick={toggleOpen} {@attach region}>{@render icon("hide")}</button>
    </div>
    <div class="row centre fade" style={style(arrangement.bottom)}>
      <div class="group across" {@attach region}>
        {#each trackpad ? [{ label: "左键", action: { kind: "mouse", right: false } } as Key, ...full] : full as key (key.label)}
          <button class="key wide" class:hold={key.action.kind === "ctrl" && ctrl} {...hold(key.action)}>{t(key.label)}</button>
        {/each}
      </div>
    </div>
  {:else}
    <div class="row centre" style={style(arrangement.bottom)}>
      <button class="grip across" aria-label={t("展开")} onclick={toggleOpen} {@attach region}><span></span></button>
    </div>
  {/if}
{:else}
  {@const inner = arrangement.kind === "float" ? arrangement.inner : layout.game}
  {#if open}
    <div class="glass across fade" style="left: {inner.x}px; top: {inner.y}px" {@attach region}>
      {#each [["library", "游戏库", onlibrary], ["menu", "菜单", onmenu], ["perf", "性能", toggleHud]] as const as [name, label, action] (name)}
        <button class="ico" class:on={name === "perf" && prefs.showFps} aria-label={t(label)} onclick={action}>{@render icon(name)}</button>
      {/each}
    </div>
    <div class="glass panel fade" style="right: {layout.width - inner.x - inner.width}px; top: {inner.y + inner.height / 2}px" {@attach region}>
      <div class="end"><button class="ico" aria-label={t("收起")} onclick={toggleOpen} style="height: 36px">{@render icon("hide")}</button></div>
      <div class="keys two">
        {#each trackpad ? [{ label: "左键", action: { kind: "mouse", right: false } } as Key, ...full] : full as key (key.label)}
          {@render keyButton(key, 40)}
        {/each}
      </div>
    </div>
  {:else}
    <button class="edge" aria-label={t("展开")} onclick={toggleOpen}
      style="right: {layout.width - inner.x - inner.width}px; top: {inner.y + inner.height / 2}px" {@attach region}><span></span></button>
  {/if}
{/if}

<style>
  .column,
  .row {
    position: absolute;
    box-sizing: border-box;
    display: flex;
    pointer-events: none;
  }
  .column {
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }
  .column.middle {
    justify-content: center;
  }
  .row {
    align-items: center;
    gap: 4px;
  }
  .row.centre {
    justify-content: center;
  }
  .spacer {
    flex: 1;
  }
  .group {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    width: 100%;
    pointer-events: auto;
  }
  .group.labels {
    align-items: stretch;
    gap: 2px;
  }
  .group.across {
    flex-direction: row;
    width: auto;
    gap: 8px;
  }
  .keys {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 6px;
    width: 100%;
  }
  .keys.two {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
  button {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    pointer-events: auto;
    -webkit-tap-highlight-color: transparent;
    -webkit-touch-callout: none;
    user-select: none;
    -webkit-user-select: none;
    touch-action: none;
    transition: background-color 0.12s ease, color 0.12s ease, transform 0.12s ease;
  }
  button:focus-visible {
    outline: 2px solid #8fb0f5;
    outline-offset: 1px;
  }
  .key {
    display: grid;
    place-items: center;
    width: 100%;
    max-width: 64px;
    justify-self: center;
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.07);
    font-size: 13px;
    font-weight: 500;
    color: #f2f2f2;
    text-align: center;
    white-space: nowrap;
  }
  .tight .key {
    font-size: 11px;
  }
  .keys.two .key {
    max-width: none;
  }
  .key.wide {
    width: 64px;
    height: 44px;
  }
  .key:active,
  .key.on {
    background: rgba(255, 255, 255, 0.18);
  }
  .key.hold {
    background: rgba(90, 130, 230, 0.2);
    box-shadow: inset 0 0 0 1px rgba(122, 160, 240, 0.65);
    color: #d4e0fb;
  }
  .side-btn {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 44px;
    padding: 0 12px;
    border-radius: 10px;
    font-size: 13px;
    color: rgba(255, 255, 255, 0.86);
  }
  .side-icon,
  .ico {
    width: 48px;
    max-width: 100%;
    height: 44px;
    display: grid;
    place-items: center;
    border-radius: 10px;
    color: rgba(255, 255, 255, 0.86);
  }
  .ico {
    width: 44px;
    border-radius: 12px;
  }
  .side-btn:active,
  .side-icon:active,
  .ico:active {
    background: rgba(255, 255, 255, 0.1);
  }
  .side-btn.on,
  .side-icon.on,
  .ico.on {
    background: rgba(90, 130, 230, 0.26);
    color: #d4e0fb;
  }
  .hide {
    width: 44px;
    height: 44px;
    display: grid;
    place-items: center;
    border-radius: 22px;
    color: rgba(255, 255, 255, 0.55);
  }
  .hide.down :global(svg) {
    transform: rotate(90deg);
  }
  .hide:active {
    color: #fff;
    background: rgba(255, 255, 255, 0.07);
  }
  .big {
    width: 100%;
    max-width: 59px;
    height: min(132px, 38%);
    min-height: 64px;
    display: grid;
    place-items: center;
    border-radius: 14px;
    background: rgba(255, 255, 255, 0.08);
    font-size: 13px;
    font-weight: 500;
  }
  .big:active {
    background: rgba(255, 255, 255, 0.2);
  }
  .grip {
    width: 44px;
    height: 88px;
    display: grid;
    place-items: center;
  }
  .grip.across {
    width: 88px;
    height: 44px;
  }
  .grip span,
  .edge span {
    display: block;
    width: 4px;
    height: 36px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.32);
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.2);
  }
  .grip.across span {
    width: 36px;
    height: 4px;
  }
  .grip:active span,
  .edge:active span {
    background: rgba(255, 255, 255, 0.6);
  }
  .glass {
    position: absolute;
    box-sizing: border-box;
    background: rgba(18, 18, 20, 0.58);
    backdrop-filter: blur(16px) saturate(1.2);
    -webkit-backdrop-filter: blur(16px) saturate(1.2);
    border: 1px solid rgba(255, 255, 255, 0.1);
  }
  .glass.across {
    display: flex;
    gap: 2px;
    padding: 4px;
    border-radius: 16px;
  }
  .glass.panel {
    width: 140px;
    padding: 6px 8px 8px;
    border-radius: 18px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    transform: translateY(-50%);
  }
  .end {
    display: flex;
    justify-content: flex-end;
  }
  .edge {
    position: absolute;
    width: 44px;
    height: 88px;
    display: grid;
    place-items: center;
    transform: translateY(-50%);
  }
  .more {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    display: grid;
    grid-template-columns: repeat(2, 64px);
    gap: 6px;
    padding: 8px;
    border-radius: 14px;
    background: rgba(30, 30, 32, 0.92);
    border: 1px solid rgba(255, 255, 255, 0.08);
    pointer-events: auto;
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
  @media (prefers-reduced-motion: reduce) {
    .fade {
      animation: none;
    }
    button {
      transition: none;
    }
  }
</style>
