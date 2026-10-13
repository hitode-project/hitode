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
  import type { Game } from "../lib/types";

  // The 存档 dialog of the manage screen: save location, backups and backup rules.
  interface Props {
    game: Game;
    onclose: () => void;
  }

  let { game, onclose }: Props = $props();

  const info = untrack(() => fakeSaveInfo(game));
  let backups = $state<Backup[]>(info.backups);
  let auto = $state(info.autoBackup);
  let keep = $state(info.keep);
  let ask = $state<string | null>(null);
  let restored = $state<string | null>(null);
  let fresh = $state<string | null>(null);
  let made = 0;

  let dialog = $state<HTMLElement>();
  $effect(() => {
    dialog?.focus();
  });

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

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    }
  }
</script>

<button class="dlg-scrim" tabindex="-1" aria-hidden="true" onclick={onclose}></button>
<div class="dlg" role="dialog" aria-modal="true" aria-labelledby="sv-title" tabindex="-1" bind:this={dialog} {onkeydown}>
  <div class="head">
    <div style="flex: 1; min-width: 0">
      <h2 id="sv-title">存档</h2>
      <div lang="ja" class="sub">{game.title}</div>
    </div>
    <button class="icon-btn" aria-label="关闭" onclick={onclose}><Icon name="close" stroke={1.5} /></button>
  </div>

  <div class="body scroll">
    <div class="loc">
      <span style="color: var(--fg3); flex: 0 0 auto">位置</span>
      <span class="mono path">{info.path}</span>
      <button class="q" onclick={() => revealFolder(info.path)}>打开</button>
    </div>

    <section aria-labelledby="sv-bk" style="display: flex; flex-direction: column; gap: 8px">
      <h3 id="sv-bk" class="lbl">备份</h3>
      <div style="display: flex; flex-direction: column">
        {#each backups as b (b.id)}
          <div class="bk" class:ask={ask === b.id} class:new={ask !== b.id && fresh === b.id}>
            {#if ask !== b.id}
              <span style="flex: 1; min-width: 0; white-space: nowrap">{b.when}<span style="color: var(--fg3)">{` · ${b.kind}`}</span></span>
              <span class="num">{b.size}</span>
              {#if restored === b.id}
                <span class="num" role="status" style="min-width: 52px; text-align: center">已恢复</span>
              {:else}
                <button class="q" style="min-width: 52px; justify-content: center" aria-label="恢复 {b.when} 的备份" onclick={() => (ask = b.id)}>恢复</button>
              {/if}
            {:else}
              <span style="flex: 1; min-width: 0">用这个备份覆盖当前存档？</span>
              <button class="q" onclick={() => (ask = null)}>取消</button>
              <button class="primary sm" onclick={() => restore(b)}>恢复</button>
            {/if}
          </div>
        {/each}
      </div>
    </section>

    <div style="display: flex; flex-direction: column; border-top: 1px solid var(--line)">
      <div class="row">
        <span style="flex: 1">退出游戏时自动备份</span>
        <button class="sw" class:on={auto} role="switch" aria-checked={auto} aria-label="退出游戏时自动备份" onclick={() => (auto = !auto)}></button>
      </div>
      <div class="row">
        <span style="flex: 1">保留</span>
        <div class="seg" role="group" aria-label="保留备份数">
          {#each KEEP_CHOICES as k (k)}
            <button class:on={keep === k} aria-pressed={keep === k} onclick={() => (keep = k)}>{k}</button>
          {/each}
        </div>
      </div>
    </div>
  </div>

  <div class="foot">
    <button class="secondary" onclick={() => importSaves(game)}>导入…</button>
    <button class="secondary" onclick={() => exportSaves(game)}>导出…</button>
    <div style="flex: 1"></div>
    <button class="primary" onclick={makeBackup}>立即备份</button>
  </div>
</div>

<style>
  /* Tokens of the manage board that app.css does not define. */
  .dlg-scrim,
  .dlg {
    --seg-on: #ffffff;
    --knob: #ffffff;
    --off: rgba(24, 24, 22, 0.2);
    --scrim: rgba(0, 0, 0, 0.28);
  }
  :global(:root[data-theme="dark"]) .dlg-scrim,
  :global(:root[data-theme="dark"]) .dlg {
    --seg-on: #3a3a3e;
    --knob: #ececea;
    --off: rgba(255, 255, 255, 0.2);
    --scrim: rgba(0, 0, 0, 0.5);
  }
  .dlg-scrim {
    all: unset;
    position: fixed;
    inset: 0;
    z-index: 8;
    background: var(--scrim);
    cursor: default;
    animation: fade 0.16s ease-out;
  }
  .dlg {
    position: fixed;
    left: 50%;
    top: 50%;
    z-index: 9;
    transform: translate(-50%, -50%);
    width: 520px;
    max-width: calc(100% - 48px);
    max-height: calc(100% - 48px);
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    border-radius: 16px;
    background: var(--pop);
    box-shadow: 0 0 0 1px var(--line), 0 24px 64px rgba(0, 0, 0, 0.28);
    outline: none;
    animation: fade 0.16s ease-out;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 22px 18px 0 24px;
  }
  h2 {
    margin: 0;
    font-size: 18px;
    line-height: 26px;
    font-weight: 600;
  }
  .sub {
    margin-top: 2px;
    font-size: 13px;
    color: var(--fg2);
  }
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    padding: 18px 24px 4px;
    display: flex;
    flex-direction: column;
    gap: 22px;
  }
  .lbl {
    margin: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .loc {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 40px;
    padding: 0 5px 0 12px;
    border-radius: 10px;
    background: var(--field);
    font-size: 13px;
  }
  .path {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    color: var(--fg2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    word-break: normal;
  }
  /* The manage board's quiet button: a 30px pill. */
  .q {
    all: unset;
    box-sizing: border-box;
    height: 30px;
    padding: 0 10px;
    display: flex;
    align-items: center;
    border-radius: 15px;
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .q:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .bk {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 48px;
    padding: 0 6px 0 10px;
    border-radius: 9px;
    font-size: 13px;
  }
  .bk + .bk {
    box-shadow: 0 -1px 0 var(--line);
  }
  .bk.ask {
    background: var(--sel);
    box-shadow: none;
  }
  .bk.ask + .bk {
    box-shadow: none;
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
  .num {
    font-variant-numeric: tabular-nums;
    color: var(--fg3);
    font-size: 12px;
    white-space: nowrap;
  }
  .primary.sm {
    height: 30px;
    padding: 0 14px;
    border-radius: 15px;
    font-size: 13px;
  }
  .primary,
  .secondary {
    white-space: nowrap;
  }
  .secondary {
    padding: 0 16px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 16px;
    min-height: 48px;
    font-size: 13px;
  }
  .row + .row {
    border-top: 1px solid var(--line);
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    border-radius: 8px;
    background: var(--field);
    gap: 2px;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    min-width: 36px;
    height: 26px;
    padding: 0 10px;
    border-radius: 6px;
    font-size: 12px;
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
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
    transition: transform 0.15s ease;
  }
  .sw.on {
    background: var(--accent);
  }
  .sw.on::after {
    transform: translateX(16px);
    background: #ffffff;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 18px 24px 22px;
    border-top: 1px solid var(--line);
    margin-top: 14px;
  }
  .q:focus-visible,
  .seg button:focus-visible,
  .sw:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .dlg,
    .dlg-scrim,
    .bk.new {
      animation: none;
    }
    .sw,
    .sw::after {
      transition: none;
    }
  }
</style>
