<script lang="ts">
  import { untrack } from "svelte";
  import ReportBackdrop from "../components/ReportBackdrop.svelte";
  import { FAKE_FAILURE_LABEL } from "../lib/fake-report";
  import { LaunchFlow, UNSUPPORTED_REASON, type LaunchState } from "../lib/launch.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { prefs } from "../lib/prefs.svelte";

  interface Props {
    gameId: string;
    initial?: LaunchState;
  }

  let { gameId, initial }: Props = $props();

  const game = $derived(library.get(gameId));
  const flow = new LaunchFlow(() => gameId, untrack(() => initial));
</script>

<div class="launch">
  <ReportBackdrop cover={game?.cover ?? "cv1"} />

  <div class="body scroll">
    {#if game}
      <div class="cv"><div class="art {game.cover}" class:blur={game.nsfw && prefs.blurAdult && !library.revealed[game.id]}></div></div>
      <h1 lang="ja">{game.title}</h1>

      {#if flow.view === "prepare"}
        <div class="state fade">
          <div class="msg">首次准备中</div>
          <div
            class="track"
            role="progressbar"
            aria-label="首次准备"
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow={flow.pct}
            style="margin-top: 32px"
          >
            <span class="fill" style="width: {flow.pct}%"></span>
          </div>
          <div class="pct">{flow.pct}%</div>
          <div style="margin-top: 36px"><button class="btn quiet" onclick={() => nav.back()}>取消</button></div>
        </div>
      {:else if flow.view === "failed"}
        <div class="state fade">
          <div class="msg">游戏已停止</div>
          <div class="mono why">{FAKE_FAILURE_LABEL}</div>
          <div class="acts">
            <button class="btn primary" onclick={() => flow.retry()}>再试一次</button>
            <button class="btn quiet" onclick={() => flow.report()}>报告问题…</button>
            <button class="btn quiet" onclick={() => nav.back()}>返回</button>
          </div>
        </div>
      {:else}
        <div class="state fade">
          <div class="msg">暂不支持</div>
          <div class="mono why">{UNSUPPORTED_REASON}</div>
          <div class="acts">
            <button class="btn primary" onclick={() => nav.back()}>返回</button>
            <button class="btn quiet" onclick={() => flow.changeExe()}>更换主程序</button>
          </div>
        </div>
      {/if}
    {:else}
      <div class="state">
        <div class="msg">找不到这个游戏</div>
        <div class="acts"><button class="btn primary" onclick={() => nav.home()}>返回</button></div>
      </div>
    {/if}
  </div>
</div>

<style>
  .launch {
    position: relative;
    height: 100%;
    overflow: clip;
    background: var(--bg);
    color: var(--fg);
  }
  .body {
    position: relative;
    height: 100%;
    overflow: auto;
    box-sizing: border-box;
    padding: 48px 24px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
  }
  .cv {
    position: relative;
    flex: 0 0 auto;
    width: 132px;
    aspect-ratio: 5 / 7;
    border-radius: 8px;
    overflow: hidden;
    box-shadow: 0 24px 48px rgba(0, 0, 0, 0.24), 0 0 0 1px var(--line);
  }
  .art {
    position: absolute;
    inset: 0;
  }
  .art.blur {
    filter: blur(16px) saturate(0.8);
    transform: scale(1.25);
  }
  h1 {
    margin: 28px 0 0;
    font-size: 26px;
    line-height: 1.3;
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  .state {
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .msg {
    margin-top: 10px;
    font-size: 14px;
    color: var(--fg2);
  }
  .pct {
    margin-top: 10px;
    font-size: 12px;
    color: var(--fg3);
    font-variant-numeric: tabular-nums;
  }
  .why {
    margin-top: 20px;
    padding: 6px 12px;
    border-radius: 8px;
    background: var(--field);
    font-size: 12px;
    color: var(--fg2);
  }
  .acts {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    justify-content: center;
    margin-top: 36px;
  }
  /* The launch board's buttons are 40px tall. */
  .btn {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    height: 40px;
    border-radius: 20px;
    font-size: 14px;
    white-space: nowrap;
  }
  .btn.primary {
    padding: 0 22px;
  }
  .btn.quiet {
    padding: 0 16px;
  }
  .track {
    width: 240px;
    height: 3px;
    border-radius: 2px;
    background: var(--field);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--fg);
    transition: width 0.4s ease;
  }
  .fade {
    animation: fade 0.24s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fade {
      animation: none;
    }
    .fill {
      transition: none;
    }
  }
</style>
