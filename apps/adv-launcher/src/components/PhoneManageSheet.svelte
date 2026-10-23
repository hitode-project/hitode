<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Vndb from "./Vndb.svelte";
  let metadata = $state(false);
  import EditTitle from "./EditTitle.svelte";
  let editing = $state(false);
  import Icon from "./Icon.svelte";
  import { canReveal, reveal, revealC } from "../lib/reveal";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import type { Game } from "../lib/types";

  // The phone "更多操作" sheet over the game detail page.
  interface Props {
    game: Game;
    onclose: () => void;
    /** Called after the game was removed from the library. */
    onremoved: () => void;
  }

  let { game, onclose, onremoved }: Props = $props();

  let view = $state<"menu" | "confirm">("menu");

  // The back gesture closes the sheet.
  $effect(() =>
    nav.onBack(() => {
      onclose();
      return true;
    }),
  );

  let sheet = $state<HTMLElement>();
  $effect(() => {
    sheet?.focus();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    }
  }

  function go(run: () => void) {
    onclose();
    run();
  }

  function remove() {
    void library.remove(game.id);
    onremoved();
  }

</script>

{#if metadata}<Vndb {game} {onclose} />{/if}

{#if editing}<EditTitle {game} {onclose} />{/if}

<div class="pm">
  <button class="scrim" aria-label={t("关闭")} onclick={onclose}></button>
    <div class="sheet" role="dialog" aria-modal="true" aria-label={t("更多操作")} tabindex="-1" bind:this={sheet} {onkeydown}>
      <div class="grab" aria-hidden="true"></div>
      {#if view === "menu"}
        <div lang="ja" class="name">{game.title}</div>
        <div role="menu" aria-label={t("更多操作")} class="items">
          {#if canReveal(game.path)}
            <button class="si" role="menuitem" onclick={() => go(() => reveal(game.path))}>
              <span class="ic"><Icon name="folder" size={20} stroke={1.3} /></span>
              <span>{t("打开所在文件夹")}</span>
            </button>
          {/if}
          <button class="si" role="menuitem" onclick={() => metadata = true}><span>VNDB</span><span>{t("搜索／关联资料…")}</span></button>
    <button class="si" role="menuitem" onclick={() => (editing = true)}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true"><path d="M10.6 2.6l2.8 2.8-7.7 7.7-3.4.6.6-3.4z"></path></svg>
            <span>{t("编辑标题…")}</span>
          </button>
          <button class="si" role="menuitem" onclick={() => go(() => nav.go({ name: "game-settings", gameId: game.id }))}>
            <span class="ic"><Icon name="sliders" size={20} stroke={1.3} /></span>
            <span>{t("此游戏的设置…")}</span>
          </button>
          <button class="si" role="menuitemcheckbox" aria-checked={game.favorite} onclick={() => void library.toggleFavorite(game.id)}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill={game.favorite ? "currentColor" : "none"} stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true"><path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.7 3.9L8 11l-3.4 1.9.7-3.9L2.4 6.3l3.9-.5z"></path></svg>
            <span>{t("收藏")}</span>
            <span class="r ck" class:on={game.favorite}><Icon name="check" size={20} stroke={1.7} /></span>
          </button>
          <button class="si" role="menuitemcheckbox" aria-checked={game.nsfw} onclick={() => void library.toggleNsfw(game.id)}>
            <span class="ic"><Icon name="eye-off" size={20} stroke={1.3} /></span>
            <span>{t("隐藏封面")}</span>
            <span class="r ck" class:on={game.nsfw}><Icon name="check" size={20} stroke={1.7} /></span>
          </button>
          <button class="si" role="menuitem" onclick={() => { revealC(); onclose(); }}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="2.8" width="12" height="3.4" rx="1"></rect><path d="M3 6.2v6.3A1.5 1.5 0 0 0 4.5 14h7a1.5 1.5 0 0 0 1.5-1.5V6.2M6.5 9h3"></path></svg>
            <span>{t("打开 C 盘")}</span>
          </button>
          <button class="si" role="menuitem" onclick={() => go(() => nav.go({ name: "report", gameId: game.id }))}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3.5 14V2.5M3.5 3h8.2l-1.7 3 1.7 3H3.5"></path></svg>
            <span>{t("报告问题…")}</span>
          </button>
          <div class="sep" role="separator"></div>
          <button class="si danger" role="menuitem" onclick={() => (view = "confirm")}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2.5 4.5h11M6.3 4.5V3h3.4v1.5M4 4.5l.6 8.6a1 1 0 0 0 1 .9h4.8a1 1 0 0 0 1-.9L12 4.5"></path></svg>
            <span>{t("从游戏库移除")}</span>
          </button>
        </div>
        <button class="cancel" onclick={onclose}>{t("取消")}</button>
      {:else}
        <div class="confirm">
          <div class="confirm-t">{t("从游戏库移除？")}</div>
          <div class="confirm-s">{t("游戏文件不会被删除")}</div>
        </div>
        <button class="cancel danger" onclick={remove}>{t("移除")}</button>
        <button class="cancel" onclick={() => (view = "menu")}>{t("取消")}</button>
      {/if}
    </div>
</div>

<style>
  /* Tokens of the phone manage board that app.css does not define. */
  .pm {
    display: contents;
    --sheet: #ffffff;
    --press: rgba(24, 24, 22, 0.06);
    --seg-on: #ffffff;
    --knob: #ffffff;
    --off: rgba(24, 24, 22, 0.2);
    --scrim: rgba(0, 0, 0, 0.32);
  }
  :global(:root[data-theme="dark"]) .pm {
    --sheet: #1e1e20;
    --press: rgba(255, 255, 255, 0.06);
    --seg-on: #3a3a3e;
    --knob: #ececea;
    --off: rgba(255, 255, 255, 0.2);
    --scrim: rgba(0, 0, 0, 0.55);
  }

  .scrim {
    all: unset;
    position: fixed;
    inset: 0;
    z-index: 20;
    background: var(--scrim);
    cursor: pointer;
    animation: fade 0.2s ease-out;
  }
  .sheet {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 21;
    box-sizing: border-box;
    max-height: calc(100% - var(--safe-top) - 8px);
    overflow: auto;
    padding: 6px max(10px, var(--safe-right)) max(var(--safe-bottom), 16px) max(10px, var(--safe-left));
    border-radius: 22px 22px 0 0;
    background: var(--sheet);
    color: var(--fg);
    box-shadow: 0 -10px 40px rgba(0, 0, 0, 0.25);
    outline: none;
    scrollbar-width: none;
    animation: up 0.26s cubic-bezier(0.2, 0.8, 0.2, 1);
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
  .grab {
    width: 36px;
    height: 5px;
    border-radius: 3px;
    background: var(--line);
    margin: 4px auto 6px;
  }
  .name {
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 14px;
    font-size: 13px;
    color: var(--fg3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .items {
    display: flex;
    flex-direction: column;
  }
  .si {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 14px;
    width: 100%;
    height: 52px;
    padding: 0 14px;
    border-radius: 12px;
    font-size: 16px;
    color: var(--fg);
    cursor: pointer;
    white-space: nowrap;
  }
  .si:active {
    background: var(--press);
  }
  .si .ic {
    flex: 0 0 auto;
    display: grid;
    color: var(--fg2);
  }
  .si .r {
    margin-left: auto;
    display: flex;
    align-items: center;
  }
  .si.danger {
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
    margin: 4px 14px;
  }
  .cancel {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    height: 52px;
    margin-top: 8px;
    border-radius: 26px;
    background: var(--field);
    font-size: 16px;
    font-weight: 500;
    color: var(--fg);
    cursor: pointer;
  }
  .cancel.danger {
    color: var(--danger);
  }
  .confirm {
    padding: 14px 14px 10px;
    text-align: center;
  }
  .confirm-t {
    font-size: 17px;
    font-weight: 600;
  }
  .confirm-s {
    margin-top: 6px;
    font-size: 14px;
    color: var(--fg3);
  }

  .si:focus-visible, .cancel:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
</style>
