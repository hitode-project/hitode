<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import ReportBackdrop from "../components/ReportBackdrop.svelte";
  import { LaunchFlow } from "../lib/launch.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { prefs } from "../lib/prefs.svelte";

  let { gameId }: { gameId: string } = $props();

  const game = $derived(library.get(gameId));
  const flow = new LaunchFlow(() => gameId);
</script>

<div class="launch">
  <ReportBackdrop cover={game?.cover ?? "cv1"} />

  <div class="body scroll">
    {#if game}
      <div class="cv"><div class="art {game.cover}" class:blur={game.nsfw && prefs.blurAdult && !library.revealed[game.id]}></div></div>
      <h1 lang="ja">{game.title}</h1>

      {#if flow.view === "prepare"}
        <div class="state fade">
          <div class="msg">{t("正在启动")}</div>
          <div class="track" role="progressbar" aria-label={t("正在启动")} style="margin-top: 32px"><span class="fill"></span></div>
          <div style="margin-top: 36px"><button class="btn quiet" onclick={() => flow.cancel()}>{t("取消")}</button></div>
        </div>
      {:else if flow.view === "failed"}
        <div class="state fade">
          <div class="msg">{t("游戏已停止")}</div>
          <div class="mono why">{flow.reason}</div>
          <div class="acts">
            <button class="btn primary" onclick={() => flow.retry()}>{t("再试一次")}</button>
            <button class="btn quiet" onclick={() => flow.report()}>{t("报告问题…")}</button>
            <button class="btn quiet" onclick={() => nav.back()}>{t("返回")}</button>
          </div>
        </div>
      {:else}
        <div class="state fade">
          <div class="msg">{t("暂不支持")}</div>
          <div class="mono why">{flow.reason}</div>
          <div class="acts">
            <button class="btn primary" onclick={() => nav.back()}>{t("返回")}</button>
            <button class="btn quiet" onclick={() => flow.changeExe()}>{t("更换主程序")}</button>
          </div>
        </div>
      {/if}
    {:else}
      <div class="state">
        <div class="msg">{t("找不到这个游戏")}</div>
        <div class="acts"><button class="btn primary" onclick={() => nav.home()}>{t("返回")}</button></div>
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
  /* The host does not report preparation progress: an indeterminate bar. */
  .fill {
    display: block;
    width: 35%;
    height: 100%;
    background: var(--fg);
    animation: slide 1.2s ease-in-out infinite;
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(290%);
    }
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
      animation: none;
      width: 100%;
      opacity: 0.4;
    }
  }
</style>
