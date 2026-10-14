<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import PhoneSettingsHeader from "../components/PhoneSettingsHeader.svelte";
  import StatsChart from "../components/StatsChart.svelte";
  import StatsHero from "../components/StatsHero.svelte";
  import StatsRank from "../components/StatsRank.svelte";
  import { library } from "../lib/library.svelte";
  import { inTauri } from "../lib/bridge";
  import { STATS_RANGES, defaultBucket, fakeOlderDays, historyOlderDays, playLogs, rangeStats, type StatsRange } from "../lib/stats";

  let range = $state<StatsRange>("month");
  let pick = $state<number | null>(null);
  const data = $derived(rangeStats(playLogs(library.games, library.now, inTauri ? historyOlderDays : fakeOlderDays), range, new Date(library.now)));
  const selected = $derived(pick ?? defaultBucket(data.buckets));
</script>

<div class="screen">
  <PhoneSettingsHeader label={t("返回游戏库")} />

  <main class="scroll">
    <h1>{t("游玩统计")}</h1>

    <div class="seg" role="group" aria-label={t("时间范围")}>
      {#each STATS_RANGES as r (r.value)}
        <button
          class:on={range === r.value}
          aria-pressed={range === r.value}
          onclick={() => {
            range = r.value;
            pick = null;
          }}
        >
          {r.label}
        </button>
      {/each}
    </div>

    <StatsHero {data} />

    <StatsChart
      title={data.chartTitle}
      buckets={data.buckets}
      scale={data.scale}
      labelEvery={data.labelEvery}
      {selected}
      onselect={(i) => (pick = i)}
    />

    <StatsRank rows={data.rank} />
  </main>
</div>

<style>
  .screen {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  main {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 8px calc(var(--safe-right) + 20px) calc(var(--safe-bottom) + 12px) calc(var(--safe-left) + 20px);
    display: flex;
    flex-direction: column;
    gap: 24px;
    scrollbar-width: none;
  }
  main::-webkit-scrollbar {
    display: none;
  }
  h1 {
    margin: 0;
    font-size: 30px;
    line-height: 1.25;
    font-weight: 600;
  }
  .seg {
    --seg-on: #ffffff;
    flex: 0 0 auto;
    display: flex;
    padding: 2px;
    border-radius: 10px;
    background: var(--field);
    gap: 2px;
  }
  :global(:root[data-theme="dark"]) .seg {
    --seg-on: #2c2c2f;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    flex: 1;
    height: 40px;
    display: grid;
    place-items: center;
    border-radius: 8px;
    font-size: 15px;
    color: var(--fg2);
    cursor: pointer;
  }
  .seg button.on {
    background: var(--seg-on);
    color: var(--fg);
    font-weight: 500;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.1);
  }
  .seg button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
