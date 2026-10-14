<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { untrack } from "svelte";
  import ManageSaves from "./ManageSaves.svelte";
  import { device } from "../lib/device.svelte";
  import PhoneSettingsHeader from "./PhoneSettingsHeader.svelte";
  import UpscalingSelect from "./UpscalingSelect.svelte";
  import SettingsSeg from "./SettingsSeg.svelte";
  import { revealFolder, type OverrideKey } from "../lib/fake-settings";
  import { prefs } from "../lib/prefs.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { GameForm } from "../lib/settings-form.svelte";
  import type { Game } from "../lib/types";

  let { game }: { game: Game } = $props();

  let saves = $state(false);
  // The back gesture closes the saves dialog first.
  $effect(() => {
    if (!saves) return;
    return nav.onBack(() => {
      saves = false;
      return true;
    });
  });

  // The parent re-creates this form when the game changes.
  const form = new GameForm(untrack(() => game));
  const setup = $derived(form.setup);
  const fonts = GameForm.FONTS;
  const hidden = $derived(game.nsfw && prefs.blurAdult && !library.revealed[game.id]);
</script>

{#snippet head(key: OverrideKey, label: string, lang?: string)}
  <span class="t">
    <i class="dot" aria-hidden="true"></i><span {lang} style="white-space: nowrap">{label}</span>{#if form.overridden(key)}<span class="sr">{t("（已单独设置）")}</span>{/if}
  </span>
  <button class="quiet rst" aria-label={t("{0} 恢复为全局设置", label)} onclick={() => form.reset(key)}>{t("恢复")}</button>
{/snippet}

{#snippet chevron()}
  <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4"></path></svg>
{/snippet}

<div class="screen">
  <PhoneSettingsHeader />

  <main class="scroll" aria-labelledby="pg-title">
    <div>
      <h1 id="pg-title">{t("此游戏的设置")}</h1>
      <div class="game">
        <span class="cv" aria-hidden="true"><span class="art {game.cover}" class:blur={hidden}></span></span>
        <span lang="ja" class="ellipsis">{game.title}</span>
      </div>
    </div>

    {#if form.error}<p role="alert">{form.error}</p>{/if}
    <p>{t("光盘与字体设置在下次启动时生效。")}</p>

    <section class="sec" aria-labelledby="pg-run">
      <h2 id="pg-run">{t("运行")}</h2>
      <div class="row">
        <span class="t">{t("主程序")}</span>
        <span class="selw">
          <select class="mono-sel" aria-label={t("主程序")} onchange={(e) => form.setExe(e.currentTarget.value)}>
            {#each form.exes as exe (exe)}
              <option value={exe} selected={exe === form.exe}>{exe}</option>
            {/each}
          </select>
          {@render chevron()}
        </span>
      </div>
      <div class="row" style="min-height: 68px">
        <span class="t" style="display: flex; flex-direction: column; gap: 3px">
          <span>{t("编码")}</span>
          <span class="enc ellipsis" lang={game.codepage === 932 ? "ja" : "zh-CN"}>{form.encPreview}</span>
        </span>
        <SettingsSeg
          variant="phone-sheet"
          label={t("编码")}
          value={game.codepage === 936 ? "936" : "932"}
          onpick={(v) => form.setCodepage(v === "936" ? 936 : 932)}
          options={[
            { value: "932", label: t("日文") },
            { value: "936", label: t("简体中文") },
          ]}
        />
      </div>
      {#if device.gameView || device.simulated}
        <div class="row">
          <span class="t">{t("光盘")}</span>
          <span class="val" style="font-size: 12px">{t("更换光盘需重新启动游戏")}</span>
        </div>
        {#each setup.discs as disc, i (disc)}
          <div class="row sm">
            <span class="drv">{GameForm.LETTERS[i] ?? ""}</span>
            <span class="disc ellipsis">{disc}</span>
            <button class="quiet" style="margin-right: -12px" aria-label={t("移除 {0}", disc)} onclick={() => form.removeDisc(i)}>{t("移除")}</button>
          </div>
        {/each}
        <div class="row sm">
          <button class="quiet" style="margin-left: -12px" onclick={() => form.addDisc()} disabled={form.discsFull}>{t("添加光盘镜像…")}</button>
        </div>
      {/if}
    </section>

    <section class="sec" aria-labelledby="pg-view">
      <h2 id="pg-view">{t("画面")}</h2>
      {#if device.gameView || device.simulated}
        <div class="row" class:ov={form.overridden("scale")}>
          {@render head("scale", t("缩放"))}
          <SettingsSeg
            variant="phone-sheet"
            label={t("缩放")}
            value={form.value("scale")}
            onpick={(v) => form.pick("scale", v)}
            options={[
              { value: "fit", label: t("适应") },
              { value: "int", label: t("整数倍") },
              { value: "stretch", label: t("拉伸") },
            ]}
          />
        </div>
      {/if}
      <div class="row" class:ov={form.overridden("filter")}>
        {@render head("filter", t("超分辨率"))}
        <UpscalingSelect value={form.overridden("filter") ? form.value("filter") : "inherit"}
          inherit={form.global.filter}
          onpick={(v) => v === "inherit" ? form.reset("filter") : form.pick("filter", v)} />
      </div>
    </section>

    {#if device.gameView || device.simulated}
      <section class="sec" aria-labelledby="pg-font">
        <h2 id="pg-font">{t("字体")}</h2>
        {#each fonts as f (f.key)}
          {@const cur = form.value(f.key)}
          <div class="row" class:ov={form.overridden(f.key)}>
            {@render head(f.key, f.name, "ja")}
            <span class="selw">
              <select aria-label={t("{0} 的替换字体", f.name)} onchange={(e) => form.changeFont(f.key, e.currentTarget)}>
                <option value="auto" selected={cur === "auto"}>{t("自动")}</option>
                {#each form.extraFonts as name (name)}
                  <option value="font:{name}" selected={cur === `font:${name}`}>{name}</option>
                {/each}
                <option value="import">{t("导入字体文件…")}</option>
              </select>
              {@render chevron()}
            </span>
          </div>
        {/each}
        <div style="padding: 14px 0; border-bottom: 1px solid var(--line)">
          <div class="pv {form.smoothClass}" role="group" aria-label={t("字体预览")}>
            {#each fonts as f (f.key)}
              <div>
                <span lang="ja" style="font-size: 12px; color: var(--fg3)">{f.name}</span>
                <span class="pvl" lang="ja" style:font-family={GameForm.fontFamily(form.value(f.key))}>「……また、夏が来るね」</span>
              </div>
            {/each}
          </div>
        </div>
        <div class="row" class:ov={form.overridden("smooth")}>
          {@render head("smooth", t("字形平滑"))}
          <SettingsSeg
            variant="phone-sheet"
            label={t("字形平滑")}
            value={form.value("smooth")}
            onpick={(v) => form.pick("smooth", v)}
            options={[
              { value: "game", label: t("跟随游戏") },
              { value: "on", label: t("开") },
              { value: "off", label: t("关") },
            ]}
          />
        </div>
      </section>

      <section class="sec" aria-labelledby="pg-input">
        <h2 id="pg-input">{t("输入")}</h2>
        <div class="row" class:ov={form.overridden("ff")}>
          {@render head("ff", t("快进键"))}
          <SettingsSeg
            variant="phone-sheet"
            label={t("快进键")}
            value={form.value("ff")}
            onpick={(v) => form.pick("ff", v)}
            options={[
              { value: "hold", label: t("按住") },
              { value: "toggle", label: t("切换") },
            ]}
          />
        </div>

      </section>
    {/if}

    <nav aria-label={t("更多")} class="more">
      {#if device.gameView || device.simulated}
        <button class="link" onclick={() => (saves = true)}>
          <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="2.8" width="12" height="3.4" rx="1"></rect><path d="M3 6.2v6.3A1.5 1.5 0 0 0 4.5 14h7a1.5 1.5 0 0 0 1.5-1.5V6.2M6.5 9h3"></path></svg>
          <span>{t("存档…")}</span>
        </button>
      {/if}
      <button class="link" onclick={() => revealFolder(game.path)}>
        <svg class="ic" width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true"><path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"></path></svg>
        <span>{t("打开所在文件夹")}</span>
      </button>
    </nav>

    {#if device.gameView || device.simulated}
      <div style="display: flex; justify-content: center; margin-top: -8px">
        <button class="quiet" disabled={!form.anyOverridden} onclick={() => form.resetAll()}>{t("全部恢复默认")}</button>
      </div>
    {/if}
  </main>
</div>

{#if saves}
  <ManageSaves {game} onclose={() => (saves = false)} />
{/if}

<style>
  .screen {
    --mark: var(--accent);
    --font-serif-ja: "Hiragino Mincho ProN", "Yu Mincho", "Noto Serif CJK JP", "Noto Serif JP", serif;
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  :global(:root[data-theme="dark"][data-accent="blue"]) .screen {
    --mark: #6b93ee;
  }
  main {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 8px calc(var(--safe-right) + 20px) calc(var(--safe-bottom) + 12px) calc(var(--safe-left) + 20px);
    display: flex;
    flex-direction: column;
    gap: 30px;
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
  .game {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 8px;
    font-size: 15px;
    color: var(--fg2);
  }
  .cv {
    position: relative;
    flex: 0 0 auto;
    width: 20px;
    aspect-ratio: 5 / 7;
    border-radius: 3px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--line);
  }
  .art {
    position: absolute;
    inset: 0;
  }
  .art.blur {
    filter: blur(6px);
    transform: scale(1.3);
  }
  .ellipsis {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .sec {
    display: flex;
    flex-direction: column;
  }
  .sec h2 {
    margin: 0 0 2px;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 56px;
    border-bottom: 1px solid var(--line);
    font-size: 15px;
  }
  .row.sm {
    min-height: 52px;
  }
  .t {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
  }
  .dot {
    position: absolute;
    left: -12px;
    top: calc(50% - 3px);
    width: 6px;
    height: 6px;
    border-radius: 3px;
    background: var(--mark);
    opacity: 0;
    transition: opacity 0.15s ease;
  }
  .row.ov .dot {
    opacity: 1;
  }
  .row:not(.ov) .rst {
    visibility: hidden;
  }
  .val {
    font-size: 14px;
    color: var(--fg3);
    white-space: nowrap;
  }
  .enc {
    font-family: var(--font-mono);
    font-size: 13px;
    color: var(--fg2);
  }
  .drv {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 22px;
    padding: 0 7px;
    border-radius: 6px;
    background: var(--field);
    color: var(--fg2);
  }
  .disc {
    flex: 1;
    font-family: var(--font-mono);
    font-size: 13px;
  }

  .selw {
    position: relative;
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
  }
  .selw select {
    appearance: none;
    -webkit-appearance: none;
    box-sizing: border-box;
    height: 40px;
    min-width: 136px;
    max-width: 180px;
    padding: 0 30px 0 12px;
    border: 0;
    border-radius: 10px;
    background: var(--field);
    color: var(--fg);
    font-size: 14px;
    text-overflow: ellipsis;
    cursor: pointer;
  }
  .selw select.mono-sel {
    font-family: var(--font-mono);
  }
  .selw select:focus-visible {
    outline: 2px solid var(--mark);
    outline-offset: 2px;
  }
  .selw svg {
    position: absolute;
    right: 11px;
    pointer-events: none;
    color: var(--fg3);
  }

  .pv {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px;
    border-radius: 14px;
    background: var(--field);
  }
  .pvl {
    display: block;
    margin-top: 2px;
    font-size: 19px;
    line-height: 1.5;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pv.sm-on .pvl {
    -webkit-font-smoothing: antialiased;
    -moz-osx-font-smoothing: grayscale;
  }
  .pv.sm-off .pvl {
    -webkit-font-smoothing: none;
    -moz-osx-font-smoothing: auto;
  }

  .more {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--line);
  }
  .link {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    min-height: 56px;
    font-size: 15px;
    color: var(--fg);
    cursor: pointer;
    border-bottom: 1px solid var(--line);
  }
  .link:active {
    background: var(--field);
  }
  .link:focus-visible {
    outline: 2px solid var(--mark);
    outline-offset: -2px;
    border-radius: 8px;
  }
  .link .ic {
    flex: 0 0 auto;
    color: var(--fg2);
  }

  /* The board's quiet button: a 44px touch pill with a dimmed disabled state. */
  .quiet {
    box-sizing: border-box;
    height: 44px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    border-radius: 22px;
    font-size: 14px;
    white-space: nowrap;
  }
  .quiet:active {
    background: var(--field);
  }
  .quiet:focus-visible {
    outline-color: var(--mark);
    outline-offset: 2px;
  }
  .quiet[disabled] {
    cursor: default;
    color: var(--fg3);
    opacity: 0.6;
    background: transparent;
  }

  @media (prefers-reduced-motion: reduce) {
    .dot {
      transition: none;
    }
  }
</style>
