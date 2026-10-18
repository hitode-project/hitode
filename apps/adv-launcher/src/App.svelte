<script lang="ts">
  import { t, locale } from "./lib/i18n.svelte";
  import { call, onHostEvent } from "./lib/bridge";
  import JitBanner from "./components/JitBanner.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import { device } from "./lib/device.svelte";
  import { jit } from "./lib/jit.svelte";
  import { nav } from "./lib/nav.svelte";
  import { setGameActive } from "./lib/overlay";
  import { prefs } from "./lib/prefs.svelte";
  import Game from "./pages/Game.svelte";
  import GameSettings from "./pages/GameSettings.svelte";
  import Import from "./pages/Import.svelte";
  import Launch from "./pages/Launch.svelte";
  import Library from "./pages/Library.svelte";
  import Report from "./pages/Report.svelte";
  import Scan from "./pages/Scan.svelte";
  import Settings from "./pages/Settings.svelte";
  import Stats from "./pages/Stats.svelte";
  import PhoneGameSettings from "./phone/GameSettings.svelte";
  import PhoneImport from "./phone/Import.svelte";
  import PhoneLaunch from "./phone/Launch.svelte";
  import PhoneLibrary from "./phone/Library.svelte";
  import PhoneReport from "./phone/Report.svelte";
  import PhoneScan from "./phone/Scan.svelte";
  import PhoneSettings from "./phone/Settings.svelte";
  import PhoneStats from "./phone/Stats.svelte";

  let backupError = $state("");
  $effect(() => onHostEvent<string>("backup_failed", (error) => backupError = error));

  let systemDark = $state(matchMedia("(prefers-color-scheme: dark)").matches);
  $effect(() => {
    const query = matchMedia("(prefers-color-scheme: dark)");
    const update = () => (systemDark = query.matches);
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  });

  const route = $derived(nav.route);
  /** Screens drawn without the library sidebar. */
  const fullscreen = $derived(route.name === "launch" || route.name === "report");
  /**
   * Flows that keep the phone screens on touch at any width, so resizing the
   * window mid-flow restyles them instead of starting them over.
   */
  const flow = $derived(route.name === "launch" || route.name === "report" || route.name === "scan" || route.name === "import");
  const phonePage = $derived(device.phone || (device.touch && flow));
  const sidebar = $derived(!device.phone && !fullscreen);

  $effect(() => {
    const root = document.documentElement;
    root.lang = locale() === "zh" ? "zh-CN" : locale();
    root.dataset.theme = prefs.theme === "system" ? (systemDark ? "dark" : "light") : prefs.theme;
    root.dataset.accent = prefs.accent;
    root.dataset.screen = route.name;
    root.dataset.platform = device.platform;
    root.dataset.device = phonePage ? "phone" : "desktop";
    if (device.touch) root.dataset.touch = "";
    else delete root.dataset.touch;
    if (device.simulated) root.dataset.simulated = "";
  });

  $effect(() => {
    void call("set_menu_language", { language: locale() }, null).catch(console.error);
  });

  $effect(() => setGameActive(route.name === "game"));
  // A game started before JIT is ready runs interpreted to its end.
  $effect(() => {
    if (jit.active) jit.start();
  });
  $effect(() => {
    void call("set_tracking", { enabled: prefs.trackPlayTime }, null).catch(console.error);
  });
</script>

{#if backupError}<div class="backup-error" role="alert">{backupError}<button onclick={() => backupError = ""}>{t("关闭")}</button></div>{/if}

{#key locale()}
{#if route.name === "game"}
  <Game gameId={route.gameId} gameView={device.gameView} phone={device.touch} />
{:else}
  <!-- One tree for every layout: the sidebar comes and goes around the page,
       so a page both layouts share stays mounted when the window resizes.
       Phone pages in a wide window keep a centered column; launch is already
       centered over its full-bleed backdrop. -->
  <div class="shell" class:phone={!sidebar}>
    {#if sidebar}<Sidebar />{/if}
    <div
      class="content"
      class:page={sidebar}
      class:column={phonePage && !device.phone && route.name !== "launch"}
      class:inset={device.touch && !phonePage}
    >
      {#if !phonePage}<JitBanner />{/if}
      {#if route.name === "library" || route.name === "install"}
        {@const filter = route.name === "library" ? route.filter : "all"}
        {#if phonePage}<PhoneLibrary {filter} />{:else}<Library {filter} />{/if}
      {:else if route.name === "stats"}
        {#if phonePage}<PhoneStats />{:else}<Stats />{/if}
      {:else if route.name === "import"}
        {#if phonePage}<PhoneImport />{:else}<Import />{/if}
      {:else if route.name === "scan"}
        {#if phonePage}<PhoneScan />{:else}<Scan />{/if}
      {:else if route.name === "settings"}
        {#if phonePage}<PhoneSettings />{:else}<Settings />{/if}
      {:else if route.name === "game-settings"}
        {#if phonePage}<PhoneGameSettings gameId={route.gameId} />{:else}<GameSettings gameId={route.gameId} />{/if}
      {:else if route.name === "launch"}
        {#if phonePage}<PhoneLaunch gameId={route.gameId} />{:else}<Launch gameId={route.gameId} />{/if}
      {:else if route.name === "report"}
        {#if phonePage}<PhoneReport gameId={route.gameId} />{:else}<Report gameId={route.gameId} />{/if}
      {/if}
    </div>
  </div>
{/if}

{/key}

<style>
  .backup-error { position: fixed; z-index: 100; top: 40px; left: 20px; right: 20px; padding: 16px; background: var(--bg); color: var(--fg); border: 1px solid var(--line); }
  .shell {
    position: relative;
    height: 100%;
    display: flex;
    background: var(--bg);
    color: var(--fg);
    overflow: hidden;
  }
  .shell.phone {
    flex-direction: column;
  }
  .content {
    flex: 1 1 0;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .page {
    view-transition-name: page;
  }
  .column {
    align-items: center;
  }
  .column > :global(*) {
    width: 100%;
    max-width: 720px;
  }
  /* Desktop pages on a tablet or an unfolded phone: the status bar and home
     indicator. */
  .inset {
    padding: var(--safe-top) var(--safe-right) var(--safe-bottom) 0;
  }
</style>
