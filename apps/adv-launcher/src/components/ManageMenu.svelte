<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "./Icon.svelte";
  import { canReveal, reveal } from "../lib/reveal";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import type { Game } from "../lib/types";

  // The "更多操作" menu of the manage screen's detail panel.
  interface Props {
    game: Game;
    onclose: () => void;
    onsaves: () => void;
    /** The dialogs open from the page, so the menu can close under them. */
    onvndb: () => void;
    onrename: () => void;
    onremove: () => void;
  }

  let { game, onclose, onsaves, onvndb, onrename, onremove }: Props = $props();

  let confirming = $state(false);
  let menu = $state<HTMLElement>();

  function items(): HTMLElement[] {
    return menu ? Array.from(menu.querySelectorAll<HTMLElement>('[role^="menuitem"]')) : [];
  }

  // Focus moves into the menu (or its confirm step) as it opens; arrow keys reach the items.
  $effect(() => {
    void confirming;
    menu?.focus();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
      return;
    }
    const list = items();
    if (!list.length) return;
    const i = list.indexOf(e.target as HTMLElement);
    let n = -1;
    if (e.key === "ArrowDown") n = (i + 1) % list.length;
    else if (e.key === "ArrowUp") n = (i - 1 + list.length) % list.length;
    else if (e.key === "Home") n = 0;
    else if (e.key === "End") n = list.length - 1;
    if (n >= 0) {
      e.preventDefault();
      list[n]?.focus();
    }
  }

  function go(run: () => void) {
    onclose();
    run();
  }
</script>



<div class="menu" role="menu" aria-label={t("更多操作")} tabindex="-1" bind:this={menu} {onkeydown}>
  {#if !confirming}
    {#if canReveal(game.path)}
      <button class="mi" role="menuitem" onclick={() => go(() => reveal(game.path))}>
        <span class="ic"><Icon name="folder" /></span>
        <span>{t("打开所在文件夹")}</span>
      </button>
    {/if}
    <button class="mi" role="menuitem" aria-haspopup="dialog" onclick={onvndb}><span>VNDB</span><span>{t("搜索／关联资料…")}</span></button>
    <button class="mi" role="menuitem" aria-haspopup="dialog" onclick={onrename}>
      <svg class="ic" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true"><path d="M10.6 2.6l2.8 2.8-7.7 7.7-3.4.6.6-3.4z"></path></svg>
      <span>{t("编辑标题…")}</span>
    </button>
    <button class="mi" role="menuitem" onclick={() => go(() => nav.go({ name: "game-settings", gameId: game.id }))}>
      <span class="ic"><Icon name="sliders" /></span>
      <span>{t("此游戏的设置…")}</span>
    </button>
    <button class="mi" role="menuitemcheckbox" aria-checked={game.favorite} onclick={() => void library.toggleFavorite(game.id)}>
      <svg class="ic" width="16" height="16" viewBox="0 0 16 16" fill={game.favorite ? "currentColor" : "none"} stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true"><path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.7 3.9L8 11l-3.4 1.9.7-3.9L2.4 6.3l3.9-.5z"></path></svg>
      <span>{t("收藏")}</span>
      <span class="r ck" class:on={game.favorite}><Icon name="check" stroke={1.8} /></span>
    </button>
    <button class="mi" role="menuitemcheckbox" aria-checked={game.nsfw} onclick={() => void library.toggleNsfw(game.id)}>
      <span class="ic"><Icon name="eye-off" /></span>
      <span>{t("隐藏封面")}</span>
      <span class="r ck" class:on={game.nsfw}><Icon name="check" stroke={1.8} /></span>
    </button>
    <button class="mi" role="menuitem" aria-haspopup="dialog" onclick={onsaves}>
      <svg class="ic" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="2.8" width="12" height="3.4" rx="1"></rect><path d="M3 6.2v6.3A1.5 1.5 0 0 0 4.5 14h7a1.5 1.5 0 0 0 1.5-1.5V6.2M6.5 9h3"></path></svg>
      <span>{t("存档…")}</span>
    </button>
    <button class="mi" role="menuitem" onclick={() => go(() => nav.go({ name: "report", gameId: game.id }))}>
      <svg class="ic" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3.5 14V2.5M3.5 3h8.2l-1.7 3 1.7 3H3.5"></path></svg>
      <span>{t("报告问题…")}</span>
    </button>
    <div class="sep" role="separator"></div>
    <button class="mi danger" role="menuitem" onclick={() => (confirming = true)}>
      <svg class="ic" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2.5 4.5h11M6.3 4.5V3h3.4v1.5M4 4.5l.6 8.6a1 1 0 0 0 1 .9h4.8a1 1 0 0 0 1-.9L12 4.5"></path></svg>
      <span>{t("从游戏库移除")}</span>
    </button>
  {:else}
    <div style="padding: 8px 10px 4px">
      <div style="font-size: 13px; font-weight: 500">{t("从游戏库移除？")}</div>
      <div style="margin-top: 3px; font-size: 12px; color: var(--fg3)">{t("游戏文件不会被删除")}</div>
    </div>
    <div style="display: flex; justify-content: flex-end; gap: 6px; padding: 8px 4px 4px">
      <button class="mbtn" role="menuitem" onclick={() => (confirming = false)}>{t("取消")}</button>
      <button class="mbtn danger" role="menuitem" onclick={onremove}>{t("移除")}</button>
    </div>
  {/if}
</div>

<style>
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 6;
    width: 256px;
    box-sizing: border-box;
    padding: 6px;
    border-radius: 12px;
    background: var(--pop);
    box-shadow: 0 0 0 1px var(--line), 0 14px 36px rgba(0, 0, 0, 0.18), 0 2px 6px rgba(0, 0, 0, 0.06);
    display: flex;
    flex-direction: column;
    outline: none;
    animation: pop 0.14s ease-out;
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  .mi {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 33px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 13px;
    color: var(--fg);
    cursor: pointer;
    white-space: nowrap;
  }
  .mi .ic {
    flex: 0 0 auto;
    display: grid;
    color: var(--fg2);
  }
  .mi:hover {
    background: var(--hover);
  }
  .mi:focus-visible {
    background: var(--hover);
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .mi .r {
    margin-left: auto;
    display: flex;
    align-items: center;
  }
  .mi.danger {
    color: var(--danger);
  }
  .ck {
    color: var(--accent);
    opacity: 0;
  }
  .ck.on {
    opacity: 1;
  }
  .sep {
    height: 1px;
    background: var(--line);
    margin: 5px 4px;
  }
  .mbtn {
    all: unset;
    box-sizing: border-box;
    height: 30px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    border-radius: 15px;
    font-size: 13px;
    cursor: pointer;
    color: var(--fg2);
  }
  .mbtn:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .mbtn.danger {
    background: var(--field);
    color: var(--danger);
    font-weight: 500;
  }
  .mbtn.danger:hover {
    background: var(--sel);
  }
  .mbtn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .menu {
      animation: none;
    }
  }
</style>
