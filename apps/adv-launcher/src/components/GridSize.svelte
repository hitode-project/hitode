<script lang="ts">
  import { t } from "../lib/i18n.svelte";

  // Cover size of the library grid as a slider over the column counts that
  // fit: right is fewer, larger covers.
  interface Props {
    /** Column counts that fit, fewest first. */
    choices: number[];
    columns: number;
    onpick: (columns: number) => void;
  }

  let { choices, columns, onpick }: Props = $props();

  const last = $derived(choices.length - 1);
  /** Slider stop: 0 is the most columns. */
  const stop = $derived.by(() => {
    const index = choices.indexOf(columns);
    if (index >= 0) return last - index;
    return columns > (choices[last] ?? 0) ? 0 : last;
  });
  const at = (value: number) => (last > 0 ? (value / last) * 100 : 0);
</script>

<div class="size" title={t("每行 {0} 个", columns)}>
  <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
    <rect x="2" y="2" width="5" height="5" rx="1" /><rect x="9" y="2" width="5" height="5" rx="1" /><rect x="2" y="9" width="5" height="5" rx="1" /><rect x="9" y="9" width="5" height="5" rx="1" />
  </svg>
  <div class="slider">
    <div class="track">
      <div class="rail"></div>
      {#each choices as _, i (i)}<span class="tick" style:left="{at(i)}%"></span>{/each}
      <div class="thumb" style:left="{at(stop)}%"></div>
    </div>
    <input
      type="range" min="0" max={last} step="1" value={stop}
      aria-label={t("封面大小")} aria-valuetext={t("每行 {0} 个", columns)}
      oninput={(e) => onpick(choices[last - Number(e.currentTarget.value)]!)}
    />
  </div>
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true">
    <rect x="2.5" y="2.5" width="11" height="11" rx="2" />
  </svg>
</div>

<style>
  .size {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--fg3);
  }
  .slider {
    position: relative;
    width: 88px;
    height: 28px;
  }
  .track {
    position: absolute;
    inset: 0 7px;
  }
  .rail {
    position: absolute;
    left: 0;
    right: 0;
    top: 50%;
    height: 3px;
    margin-top: -1.5px;
    border-radius: 2px;
    background: var(--line);
  }
  .tick {
    position: absolute;
    top: 50%;
    width: 3px;
    height: 3px;
    margin: -1.5px 0 0 -1.5px;
    border-radius: 2px;
    background: var(--fg3);
    opacity: 0.6;
  }
  .thumb {
    position: absolute;
    top: 50%;
    width: 14px;
    height: 14px;
    margin: -7px 0 0 -7px;
    border-radius: 7px;
    background: #fff;
    box-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.12), 0 1px 3px rgba(0, 0, 0, 0.3);
    transition: left 0.2s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  input {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    cursor: pointer;
  }
  .slider:has(input:focus-visible) .thumb {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .thumb {
      transition: none;
    }
  }
</style>
