<script lang="ts">
  import { t, locale } from "./lib/i18n.svelte";
  import { call, onHostEvent } from "./lib/bridge";
  import Sidebar from "./components/Sidebar.svelte";
  import { device } from "./lib/device.svelte";
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
  const fullscreen = $derived(route.name === "launch" || route.name === "game" || route.name === "report");

  $effect(() => {
    const root = document.documentElement;
    root.lang = locale() === "zh" ? "zh-CN" : locale();
    root.dataset.theme = prefs.theme === "system" ? (systemDark ? "dark" : "light") : prefs.theme;
    root.dataset.accent = prefs.accent;
    root.dataset.screen = route.name;
    root.dataset.platform = device.platform;
    root.dataset.device = device.phone ? "phone" : "desktop";
    if (device.simulated) root.dataset.simulated = "";
  });

  $effect(() => {
    void call("set_menu_language", { language: locale() }, null).catch(console.error);
  });

  $effect(() => setGameActive(route.name === "game"));
  $effect(() => {
    void call("set_tracking", { enabled: prefs.trackPlayTime }, null).catch(console.error);
  });
</script>

{#if backupError}<div class="backup-error" role="alert">{backupError}<button onclick={() => backupError = ""}>{t("关闭")}</button></div>{/if}

{#key locale()}
{#if route.name === "game"}
  <Game gameId={route.gameId} gameView={device.gameView} phone={device.phone} />
{:else if device.phone}
  <!-- Phone screens fill the window; there is no sidebar. -->
  <div class="phone">
    {#if route.name === "library"}
      <PhoneLibrary filter={route.filter} />
    {:else if route.name === "stats"}
      <PhoneStats />
    {:else if route.name === "import"}
      <PhoneImport />
    {:else if route.name === "scan"}
      <PhoneScan />
    {:else if route.name === "install"}
      <PhoneLibrary filter="all" />
    {:else if route.name === "settings"}
      <PhoneSettings />
    {:else if route.name === "game-settings"}
      <PhoneGameSettings gameId={route.gameId} />
    {:else if route.name === "launch"}
      <PhoneLaunch gameId={route.gameId} />
    {:else if route.name === "report"}
      <PhoneReport gameId={route.gameId} />
    {/if}
  </div>
{:else if fullscreen}
  {#if route.name === "launch"}
    <Launch gameId={route.gameId} />
  {:else if route.name === "report"}
    <Report gameId={route.gameId} />
  {/if}
{:else}
  <div class="shell">
    <Sidebar />
    <div class="content">
      {#if route.name === "library"}
        <Library filter={route.filter} />
      {:else if route.name === "stats"}
        <Stats />
      {:else if route.name === "import"}
        <Import />
      {:else if route.name === "scan"}
        <Scan />
      {:else if route.name === "install"}
        <Library filter="all" />
      {:else if route.name === "settings"}
        <Settings />
      {:else if route.name === "game-settings"}
        <GameSettings gameId={route.gameId} />
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
  .phone {
    position: relative;
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--fg);
    overflow: hidden;
  }
  .content {
    view-transition-name: page;
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
</style>
