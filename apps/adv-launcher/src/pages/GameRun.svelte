<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "../components/Icon.svelte";
  import PhoneSettingsHeader from "../components/PhoneSettingsHeader.svelte";
  import { call, inTauri } from "../lib/bridge";
  import { device } from "../lib/device.svelte";
  import { fakeRegistryFiles, loadGameSetup } from "../lib/fake-settings";
  import { importRegistry, libraryInspect, scriptInput, type ExeCandidate } from "../lib/host";
  import { runs } from "../lib/launch.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { prefs } from "../lib/prefs.svelte";

  let { gameId }: { gameId: string } = $props();

  const game = $derived(library.get(gameId));
  const hidden = $derived(!!game && game.nsfw && prefs.blurAdult && !library.revealed[game.id]);

  let exes = $state<string[]>([]);
  let scripts = $state<string[]>([]);
  let loading = $state(true);
  let busy = $state<string | null>(null);
  let error = $state("");
  let notice = $state<{ file: string; entries: number } | null>(null);

  /** The script this page started for this game, while it runs or once it ended. */
  const script = $derived(game && runs.script?.gameId === game.id ? runs.script : null);
  const scriptRunning = $derived(!!script && !script.result);
  let line = $state("");
  let consoleView = $state<HTMLPreElement>();

  $effect(() => {
    // The newest output stays in view.
    void script?.output;
    if (consoleView) consoleView.scrollTop = consoleView.scrollHeight;
  });

  async function send(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const text = line;
    line = "";
    try {
      await scriptInput(text);
    } catch (e) {
      error = String(e);
    }
  }

  const mainExe = $derived(
    game ? (inTauri ? game.exe : loadGameSetup(game.id, game.codepage).exe) : "",
  );

  const orderedExes = $derived.by(() => {
    if (!game) return [];
    const main = mainExe.toLowerCase();
    const list = exes.some((name) => name.toLowerCase() === main) ? exes : [mainExe, ...exes];
    return [...list].sort((a, b) => {
      const aMain = a.toLowerCase() === main;
      const bMain = b.toLowerCase() === main;
      if (aMain !== bMain) return aMain ? -1 : 1;
      return 0;
    });
  });

  $effect(() => {
    const current = game;
    error = "";
    notice = null;
    if (!current) {
      exes = [];
      scripts = [];
      loading = false;
      return;
    }
    if (!inTauri) {
      const setup = loadGameSetup(current.id, current.codepage);
      exes = [...setup.exes];
      scripts = [...(fakeRegistryFiles[current.id] ?? [])];
      loading = false;
      return;
    }
    let alive = true;
    loading = true;
    void libraryInspect(current.path)
      .then((found) => {
        if (!alive) return;
        const candidates: ExeCandidate[] = found?.exes.filter((e) => e.arch === "x86") ?? [];
        exes = candidates.map((e) => e.path);
        scripts = found?.registryFiles ?? [];
      })
      .catch((e) => {
        if (alive) error = String(e);
      })
      .finally(() => {
        if (alive) loading = false;
      });
    return () => {
      alive = false;
    };
  });

  function isBatch(name: string): boolean {
    return /\.(bat|cmd)$/i.test(name);
  }

  function isExe(name: string): boolean {
    return /\.exe$/i.test(name);
  }

  function runExe(target: string): void {
    if (!game || busy !== null) return;
    error = "";
    if (target.toLowerCase() === mainExe.toLowerCase()) {
      nav.go({ name: "launch", gameId: game.id });
    } else {
      runs.launchExe(game.id, target);
    }
  }

  async function runScriptOrReg(path: string, label?: string): Promise<void> {
    if (!game || busy !== null || scriptRunning) return;
    error = "";
    notice = null;
    if (isBatch(path)) {
      // It runs here; a window it opens takes the game screen.
      if (inTauri) runs.runScript(game.id, path);
      return;
    }
    busy = path;
    try {
      const discs = loadGameSetup(game.id, game.codepage).discs;
      const result = inTauri
        ? await importRegistry(game.id, path, [...discs])
        : await new Promise<{ entries: number }>((resolve) =>
            setTimeout(() => resolve({ entries: 3 }), 300),
          );
      notice = {
        file: label ?? path.split(/[\\/]/).pop() ?? path,
        entries: result?.entries ?? 0,
      };
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function pickOtherFile(): Promise<void> {
    if (!game || busy !== null) return;
    error = "";
    try {
      const path = await call<string | null>("pick_game_file", { kind: "run" }, null);
      if (!path) return;
      if (isExe(path)) {
        runs.launchExe(game.id, path);
      } else {
        await runScriptOrReg(path);
      }
    } catch (e) {
      error = String(e);
    }
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape" && !e.defaultPrevented && !device.phone) {
      e.preventDefault();
      nav.back();
    }
  }}
/>

{#snippet content()}
  {#if game?.setup}
    <!-- The game's folder holds nothing to run until its setup has installed it. -->
    <p class="lead">{t("安装完成并选择主程序后才能在此运行程序。")}</p>
    <button class="quiet" onclick={() => nav.replace({ name: "install", gameId: game.id })}>{t("选择已安装的程序…")}</button>
  {:else if game}
    {#if error}<p class="err" role="alert">{error}</p>{/if}
    {#if script?.result}
      {@const name = script.file.split(/[\\/]/).pop() ?? script.file}
      {#if "error" in script.result}
        <p class="err" role="alert">{script.result.error}</p>
      {:else if script.result.entries > 0}
        <p class="ok" role="status">{t("已完成 {0}（写入 {1} 项）", name, script.result.entries)}</p>
      {:else if script.result.exitCode === 0}
        <p class="ok" role="status">{t("已运行 {0}", name)}</p>
      {:else}
        <p class="err" role="alert">{t("{0} 已退出（代码 {1}）", name, script.result.exitCode)}</p>
      {/if}
    {/if}
    {#if notice}
      <p class="ok" role="status">
        {notice.entries > 0
          ? t("已完成 {0}（写入 {1} 项）", notice.file, notice.entries)
          : t("已运行 {0}", notice.file)}
      </p>
    {/if}
    <p class="lead">{t("在此游戏的环境中运行程序、脚本或导入注册表。")}</p>

    <section class="sec" aria-labelledby="gr-exes">
      <h2 id="gr-exes">{t("程序")}</h2>
      {#if loading}
        <div class="row sm"><span class="muted">{t("正在读取文件夹…")}</span></div>
      {:else}
        {#each orderedExes as exe (exe)}
          {@const isMain = exe.toLowerCase() === mainExe.toLowerCase()}
          <div class="row sm">
            <span class="name ellipsis">{exe}</span>
            {#if isMain}<span class="tag">{t("主程序")}</span>{/if}
            <button
              class="quiet act"
              disabled={busy !== null}
              onclick={() => runExe(exe)}
            >
              {t("运行")}
            </button>
          </div>
        {/each}
      {/if}
    </section>

    {#if scripts.length > 0}
      <section class="sec" aria-labelledby="gr-scripts">
        <h2 id="gr-scripts">{t("脚本与注册表")}</h2>
        {#each scripts as file (file)}
          {@const batch = isBatch(file)}
          {@const running = busy === file || (scriptRunning && script?.file === file)}
          <div class="row sm">
            <span class="name ellipsis">{file}</span>
            {#if running && batch}
              <button class="quiet act" onclick={() => void runs.stop(game.id, true)}>{t("停止")}</button>
            {:else}
              <button
                class="quiet act"
                disabled={busy !== null || scriptRunning}
                onclick={() => void runScriptOrReg(file, file)}
              >
                {running ? t("正在导入…") : (batch ? t("运行") : t("导入"))}
              </button>
            {/if}
          </div>
        {/each}
      </section>
    {/if}

    {#if script && (scriptRunning || script.output)}
      <!-- What the script writes, and a line it reads when it asks. -->
      <div class="console">
        <pre bind:this={consoleView} class="out" aria-live="polite">{script.output}</pre>
        {#if scriptRunning}
          <form class="in" onsubmit={send}>
            <input bind:value={line} placeholder={t("输入后按回车发送")} autocomplete="off" spellcheck="false" />
          </form>
        {/if}
      </div>
    {/if}

    <section class="sec" aria-labelledby="gr-other">
      <h2 id="gr-other">{t("其他文件")}</h2>
      <div class="row">
        <button
          class="quiet pick"
          disabled={busy !== null || scriptRunning}
          onclick={() => void pickOtherFile()}
        >
          {(busy !== null && !scripts.includes(busy)) || (scriptRunning && !scripts.includes(script?.file ?? ""))
            ? t("正在运行…")
            : t("选择其他文件运行…")}
        </button>
        <span class="hint">.exe / .bat / .reg</span>
      </div>
    </section>
  {/if}
{/snippet}

{#if !game}
  <main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto; display: grid; place-items: center">
    <div class="missing">
      <h1>{t("找不到这个游戏")}</h1>
      <p>{t("它可能已从游戏库中移除。")}</p>
      <button class="secondary" onclick={() => nav.home()}>{t("返回游戏库")}</button>
    </div>
  </main>
{:else if device.phone}
  <div class="screen">
    <PhoneSettingsHeader />
    <main class="scroll phone-body" aria-labelledby="gr-title">
      <div>
        <h1 id="gr-title" class="phone-h1">{t("运行")}</h1>
        <div class="phone-game">
          <span class="cv" aria-hidden="true">
            <span
              class="art {game.cover}"
              style:background-size="cover"
              style:background-position="center"
              style:background-image={game.coverUrl ? `url("${game.coverUrl}")` : undefined}
              class:blur={hidden}
            ></span>
          </span>
          <span lang="ja" class="ellipsis">{game.title}</span>
        </div>
      </div>
      {@render content()}
    </main>
  </div>
{:else}
  <main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
    <div class="sheet" role="region" aria-labelledby="gr-title" aria-describedby="gr-game">
      <div class="top">
        <span class="th" aria-hidden="true">
          <span
            class="art {game.cover}"
            style:background-size="cover"
            style:background-position="center"
            style:background-image={game.coverUrl ? `url("${game.coverUrl}")` : undefined}
            class:blur={hidden}
          ></span>
        </span>
        <div style="flex: 1; min-width: 0">
          <h1 id="gr-title">{t("运行")}</h1>
          <div id="gr-game" lang="ja" class="ellipsis" style="font-size: 13px; color: var(--fg2)">{game.title}</div>
        </div>
        <button class="icon-btn" aria-label={t("关闭")} onclick={() => nav.back()}>
          <Icon name="close" stroke={1.5} />
        </button>
      </div>

      <div class="body">
        {@render content()}
      </div>

      <div class="foot">
        <button class="quiet" style="margin-left: -10px" onclick={() => nav.go({ name: "game-settings", gameId: game.id })}>
          {t("此游戏的设置…")}
        </button>
        <div style="flex: 1"></div>
        <button class="primary" onclick={() => nav.back()}>{t("完成")}</button>
      </div>
    </div>
  </main>
{/if}

<style>
  .screen {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .phone-body {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 4px 20px calc(40px + var(--safe-bottom));
    display: flex;
    flex-direction: column;
    gap: 28px;
  }
  .phone-h1 {
    margin: 0;
    font-size: 28px;
    line-height: 1.2;
    font-weight: 600;
    letter-spacing: -0.015em;
  }
  .phone-game {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
    font-size: 14px;
    color: var(--fg2);
  }
  .cv {
    position: relative;
    flex: 0 0 auto;
    width: 24px;
    aspect-ratio: 5 / 7;
    border-radius: 3px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--line);
  }
  .sheet {
    --pop: #ffffff;
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
  .top {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 20px 18px 16px 24px;
    border-bottom: 1px solid var(--line);
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
  .art.blur {
    filter: blur(8px) saturate(0.8);
    transform: scale(1.2);
  }
  h1 {
    margin: 0;
    font-size: 18px;
    line-height: 26px;
    font-weight: 600;
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
  .lead {
    margin: -6px 0 -10px;
    font-size: 13px;
    line-height: 1.5;
    color: var(--fg3);
  }
  .err {
    margin: 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    font-size: 13px;
    line-height: 1.45;
    color: var(--danger);
  }
  .ok {
    margin: 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--field);
    font-size: 13px;
    line-height: 1.45;
    color: var(--fg2);
  }
  .sec {
    display: flex;
    flex-direction: column;
  }
  .console {
    display: flex;
    flex-direction: column;
    border-radius: 10px;
    background: var(--field);
    overflow: hidden;
  }
  .out {
    margin: 0;
    max-height: 220px;
    min-height: 3em;
    overflow: auto;
    padding: 10px 12px;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg);
    white-space: pre-wrap;
    word-break: break-all;
  }
  .in input {
    width: 100%;
    box-sizing: border-box;
    padding: 9px 12px;
    border: 0;
    border-top: 1px solid var(--line);
    background: transparent;
    font-family: var(--font-mono);
    font-size: 13px;
    color: var(--fg);
    outline: none;
  }
  h2 {
    margin: 0 0 6px;
    font-size: 12px;
    font-weight: 500;
    letter-spacing: 0.06em;
    color: var(--fg3);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 48px;
    border-bottom: 1px solid var(--line);
  }
  .row.sm {
    min-height: 44px;
  }
  .name {
    flex: 1;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 13px;
    color: var(--fg);
  }
  .tag {
    flex: 0 0 auto;
    padding: 2px 7px;
    border-radius: 6px;
    background: var(--field);
    font-size: 11px;
    color: var(--fg3);
  }
  .hint {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg3);
  }
  .muted {
    font-size: 13px;
    color: var(--fg3);
  }
  .act {
    margin-right: -10px;
    flex: 0 0 auto;
  }
  .pick {
    margin-left: -10px;
  }
  .ellipsis {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .missing {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 32px;
    text-align: center;
  }
  .missing p {
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--fg3);
  }
</style>
