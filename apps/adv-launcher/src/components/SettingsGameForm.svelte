<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { untrack } from "svelte";
  import Icon from "./Icon.svelte";
  import { device } from "../lib/device.svelte";
  import UpscalingSelect from "./UpscalingSelect.svelte";
  import SettingsSeg from "./SettingsSeg.svelte";
  import { revealFolder, type OverrideKey } from "../lib/fake-settings";
  import { canReveal, reveal, revealC } from "../lib/reveal";
  import { nav } from "../lib/nav.svelte";
  import { GameForm } from "../lib/settings-form.svelte";
  import type { Game } from "../lib/types";

  let { game }: { game: Game } = $props();


  // The parent re-creates this form when the game changes.
  const form = new GameForm(untrack(() => game));
  const setup = $derived(form.setup);
  const fonts = GameForm.FONTS;
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape" && !e.defaultPrevented) {
      e.preventDefault();
      nav.back();
    }
  }}
/>

{#snippet head(key: OverrideKey, label: string, lang?: string)}
  <span class="t">
    <i class="dot" aria-hidden="true"></i><span {lang}>{label}</span>{#if form.overridden(key)}<span class="sr">{t("（已单独设置）")}</span>{/if}
  </span>
  <button class="quiet rst" aria-label={t("{0} 恢复为全局设置", label)} onclick={() => form.reset(key)}>{t("恢复")}</button>
{/snippet}

<main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
  <div class="sheet" role="region" aria-labelledby="gs-title" aria-describedby="gs-game">
    <div class="top">
      <span class="th" aria-hidden="true"><span class="art {game.cover}" style:background-size="cover" style:background-position="center" style:background-image={game.coverUrl ? `url("${game.coverUrl}")` : undefined}></span></span>
      <div style="flex: 1; min-width: 0">
        <h1 id="gs-title">{t("此游戏的设置")}</h1>
        <div id="gs-game" lang="ja" class="ellipsis" style="font-size: 13px; color: var(--fg2)">{game.title}</div>
      </div>
      <button class="icon-btn" aria-label={t("关闭")} onclick={() => nav.back()}><Icon name="close" stroke={1.5} /></button>
    </div>

    <div class="body">
      {#if form.error}<p role="alert">{form.error}</p>{/if}
      <p>{t("光盘与字体设置在下次启动时生效。")}</p>
      <section class="sec" aria-labelledby="gs-run">
        <h2 id="gs-run">{t("运行")}</h2>
        <div class="row">
          <span class="t">{t("主程序")}</span>
          <span class="selw">
            <select class="exe" aria-label={t("主程序")} onchange={(e) => form.setExe(e.currentTarget.value)}>
              {#each form.exes as exe (exe)}
                <option value={exe} selected={exe === form.exe}>{exe}</option>
              {/each}
            </select>
            <Icon name="chevron-down" size={12} stroke={1.6} />
          </span>
        </div>
        <div class="row" style="min-height: 64px">
          <span class="t" style="display: flex; flex-direction: column; gap: 3px">
            <span>{t("编码")}</span>
            <span class="enc ellipsis" lang={game.codepage === 932 ? "ja" : "zh-CN"}>{form.encPreview}</span>
          </span>
          <SettingsSeg
            variant="sheet"
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
              <button class="quiet" aria-label={t("移除 {0}", disc)} onclick={() => form.removeDisc(i)}>{t("移除")}</button>
            </div>
          {/each}
          <div class="row sm">
            <button class="quiet" style="margin-left: -10px" onclick={() => form.addDisc()} disabled={form.discsFull}>
              {t("添加光盘镜像…")}
            </button>
          </div>
        {/if}
      </section>

      <section class="sec" aria-labelledby="gs-view">
        <h2 id="gs-view">{t("画面")}</h2>
        {#if device.gameView || device.simulated}
          <div class="row" class:ov={form.overridden("scale")}>
            {@render head("scale", t("缩放"))}
            <SettingsSeg
              variant="sheet"
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
        <section class="sec" aria-labelledby="gs-font">
          <h2 id="gs-font">{t("字体")}</h2>
          {#each fonts as f (f.key)}
            {@const cur = form.value(f.key)}
            <div class="row" class:ov={form.overridden(f.key)}>
              {@render head(f.key, f.name, "ja")}
              <svg class="arrow" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 8h10M9.5 4.5L13 8l-3.5 3.5"></path></svg>
              <span class="selw">
                <select aria-label={t("{0} 的替换字体", f.name)} onchange={(e) => form.changeFont(f.key, e.currentTarget)}>
                  <option value="auto" selected={cur === "auto"}>{t("自动")}</option>
                  {#each form.extraFonts as name (name)}
                    <option value="font:{name}" selected={cur === `font:${name}`}>{name}</option>
                  {/each}
                  <option value="import">{t("导入字体文件…")}</option>
                </select>
                <Icon name="chevron-down" size={12} stroke={1.6} />
              </span>
            </div>
          {/each}
          <div style="padding: 14px 0; border-bottom: 1px solid var(--line)">
            <div class="pv {form.smoothClass}" role="group" aria-label={t("字体预览")}>
              {#each fonts as f (f.key)}
                <div class="pvr">
                  <span lang="ja" style="font-size: 12px; color: var(--fg3); white-space: nowrap">{f.name}</span>
                  <span class="pvl" lang="ja" style:font-family={GameForm.fontFamily(form.value(f.key))}>「……また、夏が来るね」</span>
                </div>
              {/each}
            </div>
          </div>
          <div class="row" class:ov={form.overridden("smooth")}>
            {@render head("smooth", t("字形平滑"))}
            <SettingsSeg
              variant="sheet"
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

        <section class="sec" aria-labelledby="gs-input">
          <h2 id="gs-input">{t("输入")}</h2>
          <div class="row" class:ov={form.overridden("ff")}>
            {@render head("ff", t("快进键"))}
            <SettingsSeg
              variant="sheet"
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

      <nav aria-label={t("更多")} style="display: flex; gap: 4px; margin: -8px 0 0 -10px">
        {#if device.gameView || device.simulated}
          <button class="quiet" onclick={revealC}>
            <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="2.8" width="12" height="3.4" rx="1"></rect><path d="M3 6.2v6.3A1.5 1.5 0 0 0 4.5 14h7a1.5 1.5 0 0 0 1.5-1.5V6.2M6.5 9h3"></path></svg>
            <span>{t("打开 C 盘")}</span>
          </button>
        {/if}
        {#if canReveal()}
          <button class="quiet" title={game.path} disabled={game.offline !== null} onclick={() => reveal(game.path)}>
            <Icon name="folder" size={15} />
            <span>{t("在访达中显示")}</span>
          </button>
        {:else}
          <button class="quiet" onclick={() => revealFolder(game.path)}>
            <Icon name="folder" size={15} />
            <span>{t("打开所在文件夹")}</span>
          </button>
        {/if}
      </nav>
    </div>

    <div class="foot">
      {#if device.gameView || device.simulated}
        <button class="quiet" style="margin-left: -10px" disabled={!form.anyOverridden} onclick={() => form.resetAll()}>{t("全部恢复默认")}</button>
      {/if}
      <div style="flex: 1"></div>
      <button class="primary" onclick={() => nav.back()}>{t("完成")}</button>
    </div>
  </div>
</main>


<style>
  .sheet {
    --pop: #ffffff;
    --mark: var(--accent);
    --font-serif-ja: "Hiragino Mincho ProN", "Yu Mincho", "Noto Serif CJK JP", "Noto Serif JP", serif;
    position: relative;
    width: 560px;
    max-width: calc(100% - 48px);
    margin: 40px auto;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    border-radius: 16px;
    background: var(--pop);
    box-shadow: 0 0 0 1px var(--line), 0 12px 40px rgba(0, 0, 0, 0.08);
  }
  :global(:root[data-theme="dark"]) .sheet {
    --pop: #222224;
    box-shadow: 0 0 0 1px var(--line), 0 12px 40px rgba(0, 0, 0, 0.3);
  }
  :global(:root[data-theme="dark"][data-accent="blue"]) .sheet {
    --mark: #6b93ee;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 20px 18px 16px 24px;
    border-bottom: 1px solid var(--line);
  }
  h1 {
    margin: 0;
    font-size: 18px;
    line-height: 26px;
    font-weight: 600;
  }
  .th {
    position: relative;
    flex: 0 0 auto;
    width: 34px;
    aspect-ratio: 5 / 7;
    border-radius: 4px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--line);
  }
  .art {
    position: absolute;
    inset: 0;
  }
  .body {
    padding: 20px 24px;
    display: flex;
    flex-direction: column;
    gap: 28px;
  }
  .foot {
    position: sticky;
    bottom: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 16px 24px 20px;
    border-top: 1px solid var(--line);
    border-radius: 0 0 16px 16px;
    background: var(--pop);
  }
  .ellipsis {
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
    gap: 12px;
    min-height: 52px;
    border-bottom: 1px solid var(--line);
    font-size: 14px;
  }
  .row.sm {
    min-height: 44px;
  }
  .t {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
  }
  .dot {
    position: absolute;
    left: -14px;
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
    font-size: 13px;
    color: var(--fg3);
    white-space: nowrap;
  }
  .enc {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg2);
  }
  .drv {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 20px;
    padding: 0 6px;
    border-radius: 5px;
    background: var(--field);
    color: var(--fg2);
  }
  .disc {
    flex: 1;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 13px;
  }
  .arrow {
    flex: 0 0 auto;
    color: var(--fg3);
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
    height: 30px;
    min-width: 156px;
    padding: 0 30px 0 11px;
    border: 0;
    border-radius: 8px;
    background: var(--field);
    color: var(--fg);
    font-size: 13px;
    cursor: pointer;
  }
  .selw select.exe {
    font-family: var(--font-mono);
  }
  .selw select:hover {
    background: var(--sel);
  }
  .selw select:focus-visible {
    outline: 2px solid var(--mark);
    outline-offset: 2px;
  }
  .selw :global(svg) {
    position: absolute;
    right: 10px;
    pointer-events: none;
    color: var(--fg3);
  }

  .pv {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 18px;
    border-radius: 12px;
    background: var(--field);
  }
  .pvr {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    align-items: baseline;
    column-gap: 12px;
  }
  .pvl {
    font-size: 21px;
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

  /* The board's quiet button: a pill with an icon gap and a dimmed disabled state. */
  .quiet {
    box-sizing: border-box;
    height: 30px;
    padding: 0 10px;
    display: flex;
    align-items: center;
    gap: 8px;
    border-radius: 15px;
    white-space: nowrap;
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
  .primary {
    white-space: nowrap;
  }

  @media (prefers-reduced-motion: reduce) {
    .dot {
      transition: none;
    }
  }
</style>
