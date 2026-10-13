<script lang="ts">
  import type { StatsBucket, StatsScale } from "../lib/stats";

  // Column chart of play time with a readout of the selected (or hovered) column.
  interface Props {
    title: string;
    buckets: StatsBucket[];
    scale: StatsScale;
    /** Label every n-th column, counted back from the newest. */
    labelEvery: number;
    selected: number;
    onselect: (index: number) => void;
    /** Plot height in px, without the x-axis labels. */
    height?: number;
  }

  let { title, buckets, scale, labelEvery, selected, onselect, height = 160 }: Props = $props();

  /** Column under a mouse pointer; previews its value without selecting it. */
  let hover = $state<number | null>(null);
  let cols = $state<HTMLButtonElement[]>([]);

  const shown = $derived(buckets[hover ?? selected] ?? buckets.at(-1));
  const xlabels = $derived.by(() => {
    const out: { pos: number; label: string }[] = [];
    for (let i = buckets.length - 1; i >= 0; i -= Math.max(1, labelEvery)) {
      out.push({ pos: ((i + 0.5) / buckets.length) * 100, label: buckets[i]?.short ?? "" });
    }
    return out;
  });

  function onkeydown(e: KeyboardEvent): void {
    const last = buckets.length - 1;
    const next =
      e.key === "ArrowLeft" ? selected - 1
      : e.key === "ArrowRight" ? selected + 1
      : e.key === "Home" ? 0
      : e.key === "End" ? last
      : null;
    if (next === null) return;
    e.preventDefault();
    const index = Math.min(last, Math.max(0, next));
    onselect(index);
    cols[index]?.focus();
  }
</script>

<section class="chart" aria-label={title}>
  <div class="readout" aria-live="polite">
    <span class="rl">{shown?.label ?? ""}</span>
    <span class="rv">{(shown?.hours ?? 0).toFixed(1)} 小时</span>
  </div>
  <div class="frame">
    <div class="plot" style:height="{height}px">
      {#each scale.ticks as t (t.value)}
        <div class="tick" style:bottom="{(t.value / scale.max) * 100}%"><span>{t.label}</span></div>
      {/each}
      <!-- Arrow keys move the selection; only the selected column is a tab stop. -->
      <div class="cols" role="group" aria-label={title} onpointerleave={() => (hover = null)}>
        {#each buckets as b, i (i)}
          <button
            bind:this={cols[i]}
            class="col"
            class:on={i === selected}
            class:hover={i === hover}
            tabindex={i === selected ? 0 : -1}
            aria-pressed={i === selected}
            aria-label="{b.label} {b.hours.toFixed(1)} 小时"
            onclick={() => onselect(i)}
            {onkeydown}
            onpointerenter={(e) => {
              if (e.pointerType === "mouse") hover = i;
            }}
          >
            <span class="bar" style:height="{Math.min(100, (b.hours / scale.max) * 100)}%"></span>
          </button>
        {/each}
      </div>
      <div class="xaxis" aria-hidden="true">
        {#each xlabels as x (x.pos)}
          <span class="xl" style:left="{x.pos}%">{x.label}</span>
        {/each}
      </div>
    </div>
  </div>
</section>

<style>
  .chart {
    --chart-hi: var(--chart);
    --grid: var(--line);
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  :global(:root[data-theme="dark"]) .chart {
    --chart-hi: #7da0f0;
  }
  .readout {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-variant-numeric: tabular-nums;
  }
  .rl {
    font-size: 13px;
    color: var(--fg3);
  }
  .rv {
    font-size: 15px;
    font-weight: 600;
  }
  .frame {
    padding: 0 0 24px 28px;
  }
  .plot {
    position: relative;
  }
  .tick {
    position: absolute;
    left: 0;
    right: 0;
    height: 0;
    border-top: 1px solid var(--grid);
  }
  .tick span {
    position: absolute;
    right: calc(100% + 6px);
    top: -8px;
    font-size: 11px;
    color: var(--fg3);
    font-variant-numeric: tabular-nums;
  }
  .cols {
    position: absolute;
    inset: 0;
    display: flex;
    gap: 2px;
  }
  .col {
    all: unset;
    position: relative;
    flex: 1 1 0;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    cursor: pointer;
  }
  .col:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-radius: 3px;
  }
  .bar {
    display: block;
    width: 72%;
    max-width: 22px;
    border-radius: 3px 3px 0 0;
    background: var(--chart);
    opacity: 0.55;
    transition: opacity 0.12s ease, background 0.12s ease;
  }
  .col.hover .bar {
    opacity: 0.8;
  }
  .col.on .bar {
    opacity: 1;
    background: var(--chart-hi);
  }
  .xaxis {
    position: absolute;
    left: 0;
    right: 0;
    top: 100%;
  }
  .xl {
    position: absolute;
    top: 6px;
    transform: translateX(-50%);
    font-size: 11px;
    color: var(--fg3);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  @media (prefers-reduced-motion: reduce) {
    .bar {
      transition: none;
    }
  }
</style>
