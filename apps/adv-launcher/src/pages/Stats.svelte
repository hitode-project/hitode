<script lang="ts">
  import SettingsSeg from "../components/SettingsSeg.svelte";
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

<main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
  <div class="page">
    <div class="head">
      <h1>游玩统计</h1>
      <SettingsSeg
        label="时间范围"
        value={range}
        onpick={(v) => {
          range = v;
          pick = null;
        }}
        options={STATS_RANGES}
      />
    </div>

    <StatsHero {data} />

    <StatsChart
      title={data.chartTitle}
      buckets={data.buckets}
      scale={data.scale}
      labelEvery={data.labelEvery}
      {selected}
      onselect={(i) => (pick = i)}
      height={220}
    />

    <StatsRank rows={data.rank} thumb={30} />
  </div>
</main>

<style>
  .page {
    max-width: 720px;
    margin: 0 auto;
    padding: 64px 32px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 32px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-size: 30px;
    line-height: 1.25;
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  .head :global(.seg button) {
    min-width: 52px;
  }
</style>
