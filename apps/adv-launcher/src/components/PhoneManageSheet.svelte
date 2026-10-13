<script lang="ts">
  import { untrack } from "svelte";
  import Icon from "./Icon.svelte";
  import {
    exportSaves,
    fakeSaveInfo,
    importSaves,
    KEEP_CHOICES,
    restoreBackup,
    revealFolder,
    withNewBackup,
    type Backup,
  } from "../lib/fake-manage";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import type { Game } from "../lib/types";

  // The phone "更多操作" sheet over the game detail page, and the 存档 page it opens.
  interface Props {
    game: Game;
    onclose: () => void;
    /** Called after the game was removed from the library. */
    onremoved: () => void;
  }

  let { game, onclose, onremoved }: Props = $props();

  let view = $state<"menu" | "confirm" | "saves">("menu");

  // The back gesture closes the sheet, or leaves the saves page for the detail page.
  $effect(() =>
    nav.onBack(() => {
      onclose();
      return true;
    }),
  );

  let sheet = $state<HTMLElement>();
  let page = $state<HTMLElement>();
  $effect(() => {
    if (view === "saves") page?.focus();
    else sheet?.focus();
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

  // 存档 page state, as in the desktop saves dialog.
  const info = untrack(() => fakeSaveInfo(game));
  let backups = $state<Backup[]>(info.backups);
  let auto = $state(info.autoBackup);
  let keep = $state(info.keep);
  let ask = $state<string | null>(null);
  let restored = $state<string | null>(null);
  let fresh = $state<string | null>(null);
  let made = 0;

  function openSaves() {
    ask = null;
    view = "saves";
  }

  function makeBackup() {
    made += 1;
    backups = withNewBackup(backups, made);
    fresh = backups[0]?.id ?? null;
    ask = null;
  }

  function restore(b: Backup) {
    restoreBackup(game, b);
    ask = null;
    restored = b.id;
  }
</script>

<div class="pm">
  {#if view !== "saves"}
    <button class="scrim" aria-label="关闭" onclick={onclose}></button>
    <div class="sheet" role="dialog" aria-modal="true" aria-label="更多操作" tabindex="-1" bind:this={sheet} {onkeydown}>
      <div class="grab" aria-hidden="true"></div>
      {#if view === "menu"}
        <div lang="ja" class="name">{game.title}</div>
        <div role="menu" aria-label="更多操作" class="items">
          <button class="si" role="menuitem" onclick={() => go(() => revealFolder(game.path))}>
            <span class="ic"><Icon name="folder" size={20} stroke={1.3} /></span>
            <span>打开所在文件夹</span>
          </button>
          <button class="si" role="menuitem" onclick={onclose}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true"><path d="M10.6 2.6l2.8 2.8-7.7 7.7-3.4.6.6-3.4z"></path></svg>
            <span>编辑资料…</span>
          </button>
          <button class="si" role="menuitem" onclick={() => go(() => nav.go({ name: "game-settings", gameId: game.id }))}>
            <span class="ic"><Icon name="sliders" size={20} stroke={1.3} /></span>
            <span>此游戏的设置…</span>
          </button>
          <button class="si" role="menuitemcheckbox" aria-checked={game.favorite} onclick={() => void library.toggleFavorite(game.id)}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill={game.favorite ? "currentColor" : "none"} stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true"><path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.7 3.9L8 11l-3.4 1.9.7-3.9L2.4 6.3l3.9-.5z"></path></svg>
            <span>收藏</span>
            <span class="r ck" class:on={game.favorite}><Icon name="check" size={20} stroke={1.7} /></span>
          </button>
          <button class="si" role="menuitemcheckbox" aria-checked={game.nsfw} onclick={() => void library.toggleNsfw(game.id)}>
            <span class="ic"><Icon name="eye-off" size={20} stroke={1.3} /></span>
            <span>隐藏封面</span>
            <span class="r ck" class:on={game.nsfw}><Icon name="check" size={20} stroke={1.7} /></span>
          </button>
          <button class="si" role="menuitem" aria-haspopup="dialog" onclick={openSaves}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="2.8" width="12" height="3.4" rx="1"></rect><path d="M3 6.2v6.3A1.5 1.5 0 0 0 4.5 14h7a1.5 1.5 0 0 0 1.5-1.5V6.2M6.5 9h3"></path></svg>
            <span>存档…</span>
          </button>
          <button class="si" role="menuitem" onclick={() => go(() => nav.go({ name: "report", gameId: game.id }))}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3.5 14V2.5M3.5 3h8.2l-1.7 3 1.7 3H3.5"></path></svg>
            <span>报告问题…</span>
          </button>
          <div class="sep" role="separator"></div>
          <button class="si danger" role="menuitem" onclick={() => (view = "confirm")}>
            <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2.5 4.5h11M6.3 4.5V3h3.4v1.5M4 4.5l.6 8.6a1 1 0 0 0 1 .9h4.8a1 1 0 0 0 1-.9L12 4.5"></path></svg>
            <span>从游戏库移除</span>
          </button>
        </div>
        <button class="cancel" onclick={onclose}>取消</button>
      {:else}
        <div class="confirm">
          <div class="confirm-t">从游戏库移除？</div>
          <div class="confirm-s">游戏文件不会被删除</div>
        </div>
        <button class="cancel danger" onclick={remove}>移除</button>
        <button class="cancel" onclick={() => (view = "menu")}>取消</button>
      {/if}
    </div>
  {:else}
    <div class="page" role="dialog" aria-modal="true" aria-labelledby="ps-title" tabindex="-1" bind:this={page} {onkeydown}>
      <header class="phead">
        <button class="back" aria-label="返回" onclick={onclose}>
          <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9.5 3.5L5 8l4.5 4.5"></path></svg>
        </button>
      </header>
      <main class="pbody">
        <div>
          <h1 id="ps-title">存档</h1>
          <div lang="ja" class="sub">{game.title}</div>
        </div>

        <div class="row loc">
          <span class="t loc-t">
            <span class="loc-k">位置</span>
            <span class="mono path">{info.path}</span>
          </span>
          <button class="quiet q" onclick={() => revealFolder(info.path)}>打开</button>
        </div>

        <section class="sec" aria-labelledby="ps-bk">
          <h2 id="ps-bk">备份</h2>
          {#each backups as b (b.id)}
            <div class="bk" class:ask={ask === b.id} class:new={ask !== b.id && fresh === b.id}>
              {#if ask !== b.id}
                <div class="bk-row">
                  <span class="bk-info">
                    <span class="bk-when">{b.when}<span class="dim">{` · ${b.kind}`}</span></span>
                    <span class="num">{b.size}</span>
                  </span>
                  {#if restored === b.id}
                    <span class="done" role="status">已恢复</span>
                  {:else}
                    <button class="quiet q" aria-label="恢复 {b.when} 的备份" onclick={() => (ask = b.id)}>恢复</button>
                  {/if}
                </div>
              {:else}
                <span class="bk-when">用这个备份覆盖当前存档？</span>
                <div class="bk-acts">
                  <button class="quiet" onclick={() => (ask = null)}>取消</button>
                  <button class="primary sm" onclick={() => restore(b)}>恢复</button>
                </div>
              {/if}
            </div>
          {/each}
        </section>

        <section>
          <div class="row" style="border-top: 1px solid var(--line)">
            <span class="t">退出游戏时自动备份</span>
            <button class="sw" class:on={auto} role="switch" aria-checked={auto} aria-label="退出游戏时自动备份" onclick={() => (auto = !auto)}></button>
          </div>
          <div class="row">
            <span class="t">保留</span>
            <div class="seg" role="group" aria-label="保留备份数">
              {#each KEEP_CHOICES as k (k)}
                <button class:on={keep === k} aria-pressed={keep === k} onclick={() => (keep = k)}>{k}</button>
              {/each}
            </div>
          </div>
        </section>
      </main>
      <div class="pfoot">
        <button class="secondary" onclick={() => exportSaves(game)}>导出…</button>
        <button class="secondary" onclick={() => importSaves(game)}>导入…</button>
        <button class="primary" style="flex: 1" onclick={makeBackup}>立即备份</button>
      </div>
    </div>
  {/if}
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

  .page {
    position: fixed;
    inset: 0;
    z-index: 22;
    background: var(--bg);
    color: var(--fg);
    display: flex;
    flex-direction: column;
    outline: none;
    animation: in 0.26s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  @keyframes in {
    from {
      transform: translateX(24px);
      opacity: 0;
    }
    to {
      transform: none;
      opacity: 1;
    }
  }
  .phead {
    flex: 0 0 auto;
    padding: calc(var(--safe-top) + 6px) max(8px, var(--safe-right)) 0 max(8px, var(--safe-left));
  }
  .back {
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
  .pbody {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 8px max(20px, var(--safe-right)) 24px max(20px, var(--safe-left));
    display: flex;
    flex-direction: column;
    gap: 28px;
    scrollbar-width: none;
  }
  .pbody::-webkit-scrollbar {
    display: none;
  }
  h1 {
    margin: 0;
    font-size: 30px;
    line-height: 1.25;
    font-weight: 600;
  }
  .sub {
    margin-top: 4px;
    font-size: 15px;
    color: var(--fg2);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 56px;
    border-bottom: 1px solid var(--line);
  }
  .row .t {
    flex: 1 1 auto;
    min-width: 0;
    font-size: 15px;
  }
  .loc {
    border-top: 1px solid var(--line);
    min-height: 64px;
  }
  .loc-t {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .loc-k {
    font-size: 12px;
    color: var(--fg3);
  }
  .path {
    font-size: 13px;
    color: var(--fg2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    word-break: normal;
  }
  /* The phone board's quiet button: a 44px pill. */
  .quiet {
    box-sizing: border-box;
    height: 44px;
    padding: 0 14px;
    display: flex;
    align-items: center;
    border-radius: 22px;
    font-size: 14px;
    white-space: nowrap;
  }
  .quiet:hover {
    background: none;
    color: var(--fg2);
  }
  .quiet:active {
    background: var(--press);
  }
  .q {
    margin-right: -10px;
  }
  .sec h2 {
    margin: 0 0 2px;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .bk {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 10px;
    min-height: 64px;
    padding: 8px 0;
    box-sizing: border-box;
    border-bottom: 1px solid var(--line);
  }
  .bk.ask {
    background: var(--sel);
    margin: 0 -12px;
    padding: 12px;
    border-radius: 14px;
    border-bottom-color: transparent;
  }
  .bk.new {
    animation: flash 1.2s ease-out;
  }
  @keyframes flash {
    from {
      background: var(--sel);
    }
    to {
      background: transparent;
    }
  }
  .bk-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .bk-info {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .bk-when {
    font-size: 15px;
  }
  .dim {
    color: var(--fg3);
  }
  .num {
    font-size: 12px;
    color: var(--fg3);
    font-variant-numeric: tabular-nums;
  }
  .done {
    font-size: 14px;
    color: var(--fg3);
    padding: 0 4px;
  }
  .bk-acts {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .primary,
  .secondary {
    height: 48px;
    border-radius: 24px;
    font-size: 15px;
    white-space: nowrap;
  }
  .primary {
    padding: 0 20px;
  }
  .secondary {
    padding: 0 18px;
  }
  .primary.sm {
    height: 44px;
    padding: 0 18px;
    border-radius: 22px;
    font-size: 14px;
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    border-radius: 10px;
    background: var(--field);
    gap: 2px;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    min-width: 44px;
    height: 36px;
    padding: 0 12px;
    border-radius: 8px;
    font-size: 14px;
    text-align: center;
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
  .sw {
    all: unset;
    box-sizing: border-box;
    position: relative;
    flex: 0 0 auto;
    width: 48px;
    height: 28px;
    border-radius: 14px;
    background: var(--off);
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .sw::after {
    content: "";
    position: absolute;
    top: 3px;
    left: 3px;
    width: 22px;
    height: 22px;
    border-radius: 11px;
    background: var(--knob);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
    transition: transform 0.15s ease;
  }
  .sw.on {
    background: var(--accent);
  }
  .sw.on::after {
    transform: translateX(20px);
    background: #ffffff;
  }
  .pfoot {
    flex: 0 0 auto;
    display: flex;
    gap: 8px;
    padding: 12px max(20px, var(--safe-right)) max(var(--safe-bottom), 16px) max(20px, var(--safe-left));
    border-top: 1px solid var(--line);
  }
  .back:focus-visible,
  .cancel:focus-visible,
  .seg button:focus-visible,
  .sw:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .si:focus-visible {
    background: var(--press);
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .scrim,
    .sheet,
    .page,
    .bk.new {
      animation: none;
    }
    .sw,
    .sw::after {
      transition: none;
    }
  }
</style>
