<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { device } from "../lib/device.svelte";
  import { preview } from "../lib/bridge";
  import PhoneSettingsHeader from "../components/PhoneSettingsHeader.svelte";
  import SettingsAccent from "../components/SettingsAccent.svelte";
  import UpscalingSelect from "../components/UpscalingSelect.svelte";
  import JitRow from "../components/JitRow.svelte";
  import { jit } from "../lib/jit.svelte";
  import SettingsSeg from "../components/SettingsSeg.svelte";
  import SettingsSwitch from "../components/SettingsSwitch.svelte";
  import { reportContents } from "../lib/fake-settings";
  import { nav } from "../lib/nav.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { general, setPref as set } from "../lib/settings-form.svelte";

  let contentsOpen = $state(false);
</script>

<div class="screen">
  <PhoneSettingsHeader label={t("返回游戏库")} />

  <main class="scroll">
    <h1>{t("设置")}</h1>

    <section class="sec" aria-labelledby="p-look">
      <h2 id="p-look">{t("外观")}</h2>
      <div class="row">
        <span class="t">{t("主题")}</span>
        <SettingsSeg
          variant="phone"
          label={t("主题")}
          value={prefs.theme}
          onpick={(v) => set("theme", v)}
          options={[
            { value: "system", label: t("自动") },
            { value: "light", label: t("浅色") },
            { value: "dark", label: t("深色") },
          ]}
        />
      </div>
      <div class="row">
        <span class="t">{t("语言")}</span>
        <SettingsSeg
          variant="phone"
          label={t("语言")}
          value={prefs.language}
          onpick={(v) => set("language", v)}
          options={[
            { value: "auto", label: t("自动"), lang: "zh-CN" },
            { value: "zh", label: t("中文"), lang: "zh-CN" },
            { value: "ja", label: t("日本語"), lang: "ja" },
            { value: "en", label: "English", lang: "en" },
          ]}
        />
      </div>
      <div class="row">
        <span class="t">{t("强调色")}</span>
        <SettingsAccent touch />
      </div>
    </section>

    <section class="sec" aria-labelledby="p-play">
      <h2 id="p-play">{t("游戏中")}</h2>
      {#if jit.active}
        <JitRow />
      {/if}
      {#if device.gameView || preview}
        <div class="row">
          <span class="t">{t("缩放")}</span>
          <SettingsSeg
            variant="phone"
            label={t("缩放")}
            value={prefs.scale}
            onpick={(v) => set("scale", v)}
            options={[
              { value: "fit", label: t("适应") },
              { value: "int", label: t("整数倍") },
            ]}
          />
        </div>
      {/if}
      <div class="row">
        <span class="t">{t("超分辨率")}</span>
        <UpscalingSelect value={prefs.filter} onpick={(v) => { if (v !== "inherit") set("filter", v); }} />
      </div>
      {#if device.gameView || preview}
        <div class="row">
          <span class="t">{t("快进键")}</span>
          <SettingsSeg
            variant="phone"
            label={t("快进键")}
            value={prefs.fastForward}
            onpick={(v) => set("fastForward", v)}
            options={[
              { value: "hold", label: t("按住") },
              { value: "toggle", label: t("切换") },
            ]}
          />
        </div>
        <div class="row">
          <span class="t">{t("切到后台时暂停")}</span>
          <SettingsSwitch touch label={t("切到后台时暂停")} checked={prefs.pauseInBackground} onchange={(v) => set("pauseInBackground", v)} />
        </div>
        <div class="row">
          <span class="t">{t("显示帧率")}</span>
          <SettingsSwitch touch label={t("显示帧率")} checked={prefs.showFps} onchange={(v) => set("showFps", v)} />
        </div>
      {/if}
    </section>

    <section class="sec" aria-labelledby="p-lib">
      <h2 id="p-lib">{t("游戏库")}</h2>
        <div class="row">
          <span class="t">{t("导入时自动搜索 VNDB")}</span>
          <SettingsSwitch touch label={t("导入时自动搜索 VNDB")} checked={prefs.fetchVndb} onchange={(v) => set("fetchVndb", v)} />
        </div>
        <p class="credit">{t("资料来源：VNDB；封面版权属于原权利人。")}</p>


      <div class="row">
        <span class="t">{t("遮挡成人内容封面")}</span>
        <SettingsSwitch touch label={t("遮挡成人内容封面")} checked={prefs.blurAdult} onchange={(v) => set("blurAdult", v)} />
      </div>
      <div class="row">
        <span class="t">{t("记录游玩时间")}</span>
        <SettingsSwitch touch label={t("记录游玩时间")} checked={prefs.trackPlayTime} onchange={(v) => set("trackPlayTime", v)} />
      </div>
    </section>

    {#if preview}
      <section class="sec" aria-labelledby="p-roots">
        <h2 id="p-roots">{t("游戏库文件夹")}</h2>
        {#each general.roots as f (f.id)}
          <div class="row">
            <span class="t two">
              <span class="path">{f.path}</span>
              <span class="meta">{general.rootMeta(f)}</span>
            </span>
            <button class="quiet" aria-label={t("移除 {0}", f.path)} onclick={() => general.removeRoot(f.id)}>{t("移除")}</button>
          </div>
        {/each}
        <div class="row">
          <button class="quiet" style="margin-left: -12px" onclick={() => nav.go({ name: "scan" })}>{t("添加文件夹…")}</button>
        </div>
      </section>

      <section class="sec" aria-labelledby="p-store">
        <h2 id="p-store">{t("存储")}</h2>
        <div class="store">
          <span class="total">{general.totalLabel}</span>
          <div class="usage" role="img" aria-label={t("存档 128 MB，缓存 {0}", general.cacheLabel)}>
            <span style:width="{general.savesPct}%" style:background="var(--accent)"></span>
            <span style:width="{general.cachePct}%" class="cache"></span>
          </div>
          <div style="display: flex; gap: 14px; flex-wrap: wrap">
            <span class="key"><i style:background="var(--accent)"></i>{t("存档 128 MB")}</span>
            <span class="key"><i class="cache"></i>{t("缓存")} {general.cacheLabel}</span>
          </div>
        </div>
        <div class="row">
          <span class="t">{t("缓存")}</span>
          <span class="val">{general.cacheLabel}</span>
          <button class="quiet" onclick={() => general.clearCache()} disabled={general.cache !== "full"}>{general.clearLabel}</button>
        </div>
      </section>
    {/if}

    <section class="sec" aria-labelledby="p-fb">
      <h2 id="p-fb">{t("反馈与隐私")}</h2>
      {#if preview}
        {#each general.reports as r (r.id)}
          {@const gone = !!general.withdrawn[r.id]}
          <div class="row">
            <span class="t two">
              <span lang="ja" class="ellipsis">{r.title}</span>
              <span class="meta">{r.date} · {r.kind}</span>
            </span>
            <button class="quiet" onclick={() => (general.withdrawn[r.id] = true)} disabled={gone} aria-label={t("撤回 {0} 的报告", r.title)}>
              {gone ? t("已撤回") : t("撤回")}
            </button>
          </div>
        {/each}
        <div class="row">
          <span class="t two">
            <span>{t("重置报告密钥")}</span>
            <span class="meta">{t("之后的报告无法与之前的关联")}</span>
          </span>
          <button class="quiet" onclick={() => (general.keyReset = true)} disabled={general.keyReset}>
            {general.keyReset ? t("已重置") : t("重置")}
          </button>
        </div>
      {/if}
      <div class="row" class:open={contentsOpen}>
        <span class="t">{t("报告包含哪些内容")}</span>
        <button
          class="quiet"
          aria-label={contentsOpen ? t("收起报告包含哪些内容") : t("查看报告包含哪些内容")}
          aria-expanded={contentsOpen}
          aria-controls="p-fb-contents"
          onclick={() => (contentsOpen = !contentsOpen)}
        >
          {contentsOpen ? t("收起") : t("查看")}
        </button>
      </div>
      {#if contentsOpen}
        <div id="p-fb-contents" class="contents">
          <dl>
            {#each reportContents as c (c.label)}
              <div>
                <dt>{t(c.label)}</dt>
                <dd>{t(c.note)}</dd>
              </div>
            {/each}
          </dl>
          <p>{t("不含文件路径、用户名、截图和存档")}</p>
        </div>
      {/if}
    </section>

    <footer>hitode {device.version} · GPL-3.0-or-later</footer>
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
    padding: 8px calc(var(--safe-right) + 20px) calc(var(--safe-bottom) + 14px) calc(var(--safe-left) + 20px);
    display: flex;
    flex-direction: column;
    gap: 32px;
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
    min-height: 56px;
    border-bottom: 1px solid var(--line);
  }
  .row.open {
    border-bottom-color: transparent;
  }
  .t {
    flex: 1 1 auto;
    min-width: 0;
    font-size: 15px;
  }
  .t.two {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .path {
    font-family: var(--font-mono);
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .ellipsis {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta {
    font-size: 12px;
    color: var(--fg3);
  }
  .val {
    font-size: 13px;
    color: var(--fg3);
    font-variant-numeric: tabular-nums;
  }

  /* The board's quiet button: a 36px pill. */
  .quiet {
    box-sizing: border-box;
    height: 36px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    border-radius: 18px;
    font-size: 14px;
    white-space: nowrap;
  }
  .quiet:active {
    background: var(--field);
  }
  .quiet:focus-visible {
    outline-offset: 2px;
  }
  .quiet[disabled] {
    cursor: default;
    color: var(--fg3);
    background: transparent;
  }

  .store {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px 0 16px;
    border-bottom: 1px solid var(--line);
  }
  .total {
    font-size: 22px;
    font-weight: 600;
  }
  .usage {
    display: flex;
    height: 6px;
    border-radius: 3px;
    overflow: hidden;
    background: var(--field);
    gap: 2px;
  }
  .usage span {
    transition: width 0.3s ease;
  }
  .cache {
    background: var(--fg3);
    opacity: 0.5;
  }
  .key {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--fg2);
    font-variant-numeric: tabular-nums;
  }
  .key i {
    width: 6px;
    height: 6px;
    border-radius: 3px;
    display: block;
  }

  .contents {
    padding: 4px 16px 14px;
    border-radius: 12px;
    background: var(--field);
    font-size: 13px;
  }
  .contents dl {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 12px 0 0;
  }
  .contents dl div {
    display: grid;
    grid-template-columns: 72px 1fr;
    column-gap: 12px;
  }
  .contents dt {
    color: var(--fg2);
  }
  .contents dd {
    margin: 0;
    color: var(--fg3);
  }
  .contents p {
    margin: 12px 0 0;
    font-size: 12px;
    color: var(--fg3);
  }

  footer {
    font-size: 12px;
    color: var(--fg3);
  }

  @media (prefers-reduced-motion: reduce) {
    .usage span {
      transition: none;
    }
  }
  .credit {
    margin: 6px 0 4px;
    font-size: 11px;
    line-height: 1.5;
    color: var(--fg3);
  }
</style>
