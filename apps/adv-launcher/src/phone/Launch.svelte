<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { LaunchFlow } from "../lib/launch.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { prefs } from "../lib/prefs.svelte";

  let { gameId }: { gameId: string } = $props();

  const game = $derived(library.get(gameId));
  const flow = new LaunchFlow(() => gameId);
  const blurred = $derived(!!game && game.nsfw && prefs.blurAdult && !library.revealed[game.id]);
</script>

<div class="launch">
  <div class="bd {game?.cover ?? 'cv1'}" aria-hidden="true"></div>
  <div class="scrim" aria-hidden="true"></div>

  <div class="body">
    <div class="center">
      {#if game}
        <div class="cv"><div class="art {game.cover}" style:background-size="cover" style:background-position="center" style:background-image={game.coverUrl ? `url("${game.coverUrl}")` : undefined} class:blur={blurred}></div></div>
        <h1 lang="ja">{game.title}</h1>

        {#if flow.view === "prepare"}
          <div class="state fade">
            <div class="msg">{t("正在启动")}</div>
            <div class="track" role="progressbar" aria-label={t("正在启动")}><span class="fill"></span></div>
          </div>
        {:else if flow.view === "failed"}
          <div class="state fade">
            <div class="msg">{t("游戏已停止")}</div>
            <div class="chip mono">{flow.reason}</div>
          </div>
        {:else}
          <div class="state fade">
            <div class="msg">{t("暂不支持")}</div>
            <div class="chip mono">{flow.reason}</div>
          </div>
        {/if}
      {:else}
        <div class="msg">{t("找不到这个游戏")}</div>
      {/if}
    </div>

    {#if !game}
      <div class="acts"><button class="btn primary" onclick={() => nav.home()}>{t("返回")}</button></div>
    {:else if flow.view === "prepare"}
      <div class="acts"><button class="btn quiet" onclick={() => flow.cancel()}>{t("取消")}</button></div>
    {:else if flow.view === "failed"}
      <div class="acts fade">
        <button class="btn primary" onclick={() => flow.retry()}>{t("再试一次")}</button>
        <button class="btn quiet" onclick={() => flow.report()}>{t("报告问题…")}</button>
        <button class="btn quiet" onclick={() => nav.back()}>{t("返回")}</button>
      </div>
    {:else}
      <div class="acts fade">
        <button class="btn primary" onclick={() => nav.back()}>{t("返回")}</button>
        <button class="btn quiet" onclick={() => flow.changeExe()}>{t("更换主程序")}</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .launch {
    position: relative;
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg);
    color: var(--fg);
  }
  .bd {
    position: absolute;
    left: -30%;
    right: -30%;
    top: -10%;
    height: 620px;
    filter: blur(70px) saturate(1.1);
    opacity: 0.6;
  }
  .scrim {
    position: absolute;
    inset: 0;
    background: linear-gradient(180deg, rgba(244, 244, 242, 0.25) 0%, rgba(244, 244, 242, 0.75) 40%, #f4f4f2 70%);
  }
  :global(:root[data-theme="dark"]) .scrim {
    background: linear-gradient(180deg, rgba(20, 20, 21, 0.2) 0%, rgba(20, 20, 21, 0.75) 40%, #141415 70%);
  }
  .body {
    position: relative;
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    box-sizing: border-box;
    padding: var(--safe-top) max(24px, var(--safe-right)) max(var(--safe-bottom), 16px) max(24px, var(--safe-left));
    display: flex;
    flex-direction: column;
    scrollbar-width: none;
  }
  .center {
    flex: 1 0 auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    padding: 24px 0;
  }
  .cv {
    position: relative;
    flex: 0 0 auto;
    width: 160px;
    aspect-ratio: 5 / 7;
    border-radius: 8px;
    overflow: hidden;
    box-shadow: 0 24px 48px rgba(0, 0, 0, 0.26), 0 0 0 1px var(--line);
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
    margin: 26px 0 0;
    font-size: 24px;
    line-height: 1.3;
    font-weight: 600;
  }
  .state {
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .msg {
    margin-top: 8px;
    font-size: 14px;
    color: var(--fg2);
  }
  .track {
    width: 200px;
    height: 3px;
    margin-top: 32px;
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
  .chip {
    margin-top: 18px;
    padding: 6px 12px;
    border-radius: 8px;
    background: var(--field);
    font-size: 12px;
    color: var(--fg2);
  }
  .acts {
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  /* The phone launch board's buttons: a 50px primary and 48px quiet rows. */
  .btn.primary {
    height: 50px;
    border-radius: 25px;
    font-size: 16px;
  }
  .btn.quiet {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: center;
    height: 48px;
    padding: 0;
    border-radius: 24px;
    font-size: 15px;
  }
  .btn.quiet:hover {
    background: none;
  }
  .btn.quiet:active {
    background: var(--hover);
  }
  .btn:focus-visible {
    outline: 2px solid var(--fg);
    outline-offset: 2px;
  }
  .fade {
    animation: fade 0.24s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
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
