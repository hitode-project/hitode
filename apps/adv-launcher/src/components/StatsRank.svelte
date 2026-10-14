<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Cover from "./Cover.svelte";
  import type { StatsRankRow } from "../lib/stats";

  // "按作品": play time per game in the selected range.
  interface Props {
    rows: StatsRankRow[];
    /** Cover thumbnail width in px. */
    thumb?: number;
  }

  let { rows, thumb = 28 }: Props = $props();
</script>

<section aria-labelledby="stats-rank">
  <h2 id="stats-rank">{t("按作品")}</h2>
  {#if rows.length}
    <ol>
      {#each rows as r (r.game?.id ?? "other")}
        <li style:min-height="{thumb * 2}px">
          {#if r.game}
            <Cover game={r.game} style="flex: 0 0 auto; width: {thumb}px; border-radius: 3px; box-shadow: 0 0 0 1px var(--line)" />
          {:else}
            <span class="cvx" style:width="{thumb}px" aria-hidden="true"></span>
          {/if}
          <span class="body">
            <span class="head">
              <span class="title" class:other={!r.game} lang={r.game ? "ja" : "zh-CN"}>{r.title}</span>
              <span class="v">{r.hours.toFixed(1)} {t("小时")}</span>
            </span>
            <span class="track" aria-hidden="true"><span class:other={!r.game} style:width="{r.width}%"></span></span>
          </span>
        </li>
      {/each}
    </ol>
  {:else}
    <p>{t("这段时间没有游玩记录")}</p>
  {/if}
</section>

<style>
  h2 {
    margin: 0 0 6px;
    font-size: 14px;
    font-weight: 500;
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  li {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .cvx {
    flex: 0 0 auto;
    aspect-ratio: 5 / 7;
    border-radius: 3px;
    background: var(--field);
    box-shadow: 0 0 0 1px var(--line);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .title {
    flex: 1;
    min-width: 0;
    font-size: 14px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .title.other {
    color: var(--fg3);
  }
  .v {
    font-size: 13px;
    color: var(--fg2);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .track {
    height: 4px;
    border-radius: 2px;
    background: var(--field);
    overflow: hidden;
  }
  .track span {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: var(--chart);
  }
  .track span.other {
    background: var(--fg3);
  }
  p {
    margin: 8px 0 0;
    font-size: 13px;
    color: var(--fg3);
  }
</style>
