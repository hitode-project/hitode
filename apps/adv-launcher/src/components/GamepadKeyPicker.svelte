<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "./Icon.svelte";
  import {
    KEY_CATALOG,
    bindingCanLatch,
    bindingEquals,
    type KeyBinding,
    type KeyCategory,
  } from "../lib/key-layouts";
  import { CATEGORY_TABS, findKeyCategory, type ButtonGlyph, type GamepadButtonEntry } from "../lib/gamepad";

  let {
    glyph,
    entry,
    inline = false,
    backLabel = "返回",
    onpick,
    onlatch,
    onclear,
    onclose,
  }: {
    glyph: ButtonGlyph;
    entry: GamepadButtonEntry | undefined;
    inline?: boolean;
    backLabel?: string;
    onpick: (binding: KeyBinding) => void;
    onlatch: (latch: boolean) => void;
    onclear: () => void;
    onclose: () => void;
  } = $props();

  let activeTab = $state<KeyCategory>("common");
  let dialogEl = $state<HTMLDialogElement | null>(null);

  $effect(() => {
    activeTab = findKeyCategory(entry?.binding, KEY_CATALOG);
  });

  $effect(() => {
    if (!inline && dialogEl && !dialogEl.open) {
      dialogEl.showModal();
    }
  });

  const keys = $derived(KEY_CATALOG[activeTab]);
  const canLatch = $derived(Boolean(entry && bindingCanLatch(entry.binding)));

  function localizedKeyLabel(binding: KeyBinding): string {
    if (binding.kind === "sys" || binding.kind === "mouse" || binding.kind === "wheel") {
      return t(binding.label);
    }
    if (binding.label === "退格") return t("退格");
    return binding.label;
  }
</script>

{#snippet body()}
  <div class="head">
    <div class="head-left">
      {#if inline}
        <button
          type="button"
          class="back-btn"
          aria-label={t(backLabel)}
          title={t(backLabel)}
          onclick={onclose}
        >
          <Icon name="chevron-left" size={16} />
        </button>
      {/if}
      <span class="badge" class:round={glyph.shape === "round"} class:wide={glyph.shape === "pill"}>
        {glyph.glyph}
      </span>
      <span class="title">{t(glyph.name)}</span>
    </div>
    <div class="head-right">
      {#if canLatch && entry}
        <div class="seg" role="radiogroup" aria-label={t("触发方式")}>
          <button type="button" class:on={!entry.latch} onclick={() => onlatch(false)}>{t("按住")}</button>
          <button type="button" class:on={entry.latch} onclick={() => onlatch(true)}>{t("切换")}</button>
        </div>
      {/if}
      {#if entry}
        <button type="button" class="head-btn danger" onclick={onclear}>{t("清除")}</button>
      {/if}
      {#if !inline}
        <button type="button" class="head-btn" onclick={onclose}>{t("完成")}</button>
      {/if}
    </div>
  </div>

  <div class="tabs scroll" role="tablist">
    {#each CATEGORY_TABS as [cat, label] (cat)}
      <button
        type="button"
        role="tab"
        aria-selected={activeTab === cat}
        class="tab"
        class:on={activeTab === cat}
        onclick={() => (activeTab = cat)}
      >
        {t(label)}
      </button>
    {/each}
  </div>

  <div class="grid scroll" class:inline-grid={inline}>
    {#each keys as k (k.kind + ":" + k.label)}
      {@const selected = Boolean(entry && bindingEquals(entry.binding, k))}
      <button
        type="button"
        class="kbtn"
        class:on={selected}
        onclick={() => onpick(k)}
      >
        {localizedKeyLabel(k)}
      </button>
    {/each}
  </div>
{/snippet}

{#if inline}
  <div class="inline-picker">
    {@render body()}
  </div>
{:else}
  <dialog
    bind:this={dialogEl}
    class="picker-dialog"
    onclose={onclose}
    onclick={(e) => {
      if (e.target === dialogEl) onclose();
    }}
  >
    <div class="dialog-inner">
      {@render body()}
    </div>
  </dialog>
{/if}

<style>
  .inline-picker {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .picker-dialog {
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 14px;
    background: var(--pop);
    color: var(--fg);
    width: min(560px, calc(100vw - 32px));
    max-height: min(540px, calc(100vh - 48px));
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.35);
    overflow: hidden;
  }
  .picker-dialog::backdrop {
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(4px);
    -webkit-backdrop-filter: blur(4px);
  }
  .dialog-inner {
    display: flex;
    flex-direction: column;
    background: var(--pop);
    max-height: min(540px, calc(100vh - 48px));
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 12px 14px;
    border-bottom: 1px solid var(--line);
    flex: none;
  }
  .head-left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .head-right {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
  }
  .back-btn {
    all: unset;
    box-sizing: border-box;
    display: inline-grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    color: var(--accent);
    cursor: pointer;
    flex: none;
  }
  .back-btn:hover {
    background: var(--hover);
  }
  .back-btn:focus-visible {
    outline: 2px solid var(--accent);
  }
  .head-btn {
    all: unset;
    box-sizing: border-box;
    padding: 6px 8px;
    border-radius: 7px;
    font-size: 13px;
    color: var(--fg2);
    white-space: nowrap;
    cursor: pointer;
    flex: none;
  }
  .head-btn:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .head-btn:focus-visible {
    outline: 2px solid var(--accent);
  }
  .title {
    font-size: 14px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 12px;
    font-weight: 600;
    background: var(--field);
    border: 1px solid var(--line);
    color: var(--fg);
    flex: none;
  }
  .badge.round {
    width: 26px;
    height: 26px;
    border-radius: 50%;
  }
  .badge.wide {
    min-width: 34px;
    height: 24px;
    padding: 0 7px;
    border-radius: 7px;
  }
  .head-btn.danger {
    color: var(--danger);
  }
  .tabs {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
    overflow-x: auto;
    flex: none;
  }
  .tab {
    all: unset;
    box-sizing: border-box;
    padding: 6px 10px;
    border-radius: 8px;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg2);
    white-space: nowrap;
    cursor: pointer;
    flex: none;
  }
  .tab:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .tab.on {
    background: var(--sel);
    color: var(--fg);
    font-weight: 600;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(74px, 1fr));
    gap: 6px;
    padding: 12px;
    overflow-y: auto;
    flex: 0 1 auto;
    max-height: 380px;
    align-content: start;
  }
  .grid.inline-grid {
    grid-template-columns: repeat(auto-fill, minmax(66px, 1fr));
    flex: 1 1 0;
    min-height: 0;
    max-height: none;
  }
  .kbtn {
    all: unset;
    box-sizing: border-box;
    min-height: 36px;
    padding: 6px 6px;
    border-radius: 8px;
    background: var(--field);
    border: 1px solid transparent;
    color: var(--fg);
    font-size: 12px;
    font-weight: 500;
    text-align: center;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .kbtn:hover {
    background: var(--hover);
    border-color: var(--line);
  }
  .kbtn.on {
    background: var(--accent);
    color: var(--on-accent);
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
    height: 26px;
    padding: 0 9px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg2);
    white-space: nowrap;
    cursor: pointer;
  }
  .seg button.on {
    background: var(--pop);
    color: var(--fg);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.14);
  }
  :global(:root[data-touch]) .back-btn {
    width: 34px;
    height: 34px;
  }
  :global(:root[data-touch]) .kbtn {
    min-height: 40px;
    font-size: 13px;
  }
  :global(:root[data-touch]) .seg button {
    height: 30px;
    padding: 0 10px;
    font-size: 12.5px;
  }
</style>
