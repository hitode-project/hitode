<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "./Icon.svelte";
  import SettingsSeg from "./SettingsSeg.svelte";
  import type { LibrarySort } from "../lib/arrange";
  import { nav } from "../lib/nav.svelte";
  import { prefs, savePrefs } from "../lib/prefs.svelte";

  // The phone "排列" sheet: library order and covers per row. The grid stays
  // visible above it, so a change animates in place.
  interface Props {
    /** Column counts that fit, fewest first. */
    choices: number[];
    columns: number;
    oncolumns: (columns: number) => void;
    onclose: () => void;
  }

  let { choices, columns, oncolumns, onclose }: Props = $props();

  const sorts: { value: LibrarySort; label: string }[] = [
    { value: "recent", label: t("最近游玩") },
    { value: "custom", label: t("自定义") },
    { value: "title", label: t("标题") },
    { value: "brand", label: t("品牌") },
    { value: "year", label: t("发行年份") },
  ];

  $effect(() => nav.onBack(() => (onclose(), true)));

  let sheet = $state<HTMLElement>();
  $effect(() => {
    sheet?.focus();
  });

  function pick(sort: LibrarySort) {
    prefs.librarySort = sort;
    savePrefs();
  }
</script>

<div class="pa">
  <button class="scrim" aria-label={t("关闭")} onclick={onclose}></button>
  <div class="sheet" role="dialog" aria-modal="true" aria-label={t("排列")} tabindex="-1" bind:this={sheet}
    onkeydown={(e) => e.key === "Escape" && (e.preventDefault(), onclose())}>
    <div class="grab" aria-hidden="true"></div>
    <div class="head">{t("排序")}</div>
    <div role="radiogroup" aria-label={t("排序")}>
      {#each sorts as s (s.value)}
        <button class="si" role="radio" aria-checked={prefs.librarySort === s.value} onclick={() => pick(s.value)}>
          <span>{s.label}</span>
          <span class="ck" class:on={prefs.librarySort === s.value}><Icon name="check" size={20} stroke={1.7} /></span>
        </button>
      {/each}
    </div>
    <div class="hint">{t("按住封面拖动即可自定义顺序")}</div>
    {#if choices.length > 1}
      <div class="row">
        <span>{t("每行")}</span>
        <SettingsSeg
          label={t("每行")}
          variant="phone-sheet"
          options={choices.map((n) => ({ value: String(n), label: String(n) }))}
          value={String(columns)}
          onpick={(n) => oncolumns(Number(n))}
        />
      </div>
    {/if}
    <button class="cancel" onclick={onclose}>{t("完成")}</button>
  </div>
</div>

<style>
  .pa {
    display: contents;
    --sheet: #ffffff;
    --press: rgba(24, 24, 22, 0.06);
    --seg-on: #ffffff;
    --scrim: rgba(0, 0, 0, 0.2);
  }
  :global(:root[data-theme="dark"]) .pa {
    --sheet: #1e1e20;
    --press: rgba(255, 255, 255, 0.06);
    --seg-on: #3a3a3e;
    --scrim: rgba(0, 0, 0, 0.4);
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
    margin: 5px auto 4px;
    border-radius: 3px;
    background: var(--line);
  }
  .head {
    padding: 10px 14px 4px;
    font-size: 13px;
    color: var(--fg3);
  }
  .si {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    height: 48px;
    padding: 0 14px;
    border-radius: 12px;
    font-size: 16px;
    cursor: pointer;
  }
  .si:active {
    background: var(--press);
  }
  .ck {
    display: flex;
    color: var(--accent);
    opacity: 0;
  }
  .ck.on {
    opacity: 1;
  }
  .hint {
    padding: 2px 14px 12px;
    font-size: 13px;
    color: var(--fg3);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 52px;
    padding: 0 14px;
    border-top: 1px solid var(--line);
    font-size: 16px;
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
    cursor: pointer;
  }
  .si:focus-visible,
  .cancel:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .scrim,
    .sheet {
      animation: none;
    }
  }
</style>
