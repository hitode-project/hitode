<script lang="ts">
  import Sidebar from "./components/Sidebar.svelte";
  import { hostInfo, type HostInfo } from "./lib/bridge";
  import { nav } from "./lib/nav.svelte";
  import { setGameActive } from "./lib/overlay";
  import { prefs } from "./lib/prefs.svelte";
  import Game from "./pages/Game.svelte";
  import GameSettings from "./pages/GameSettings.svelte";
  import Import from "./pages/Import.svelte";
  import Install from "./pages/Install.svelte";
  import Launch from "./pages/Launch.svelte";
  import Library from "./pages/Library.svelte";
  import Report from "./pages/Report.svelte";
  import Scan from "./pages/Scan.svelte";
  import Settings from "./pages/Settings.svelte";
  import Stats from "./pages/Stats.svelte";

  let host = $state<HostInfo | null>(null);
  hostInfo().then((info) => (host = info));

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
    root.dataset.theme = prefs.theme === "system" ? (systemDark ? "dark" : "light") : prefs.theme;
    root.dataset.accent = prefs.accent;
    root.dataset.screen = route.name;
    if (host) root.dataset.platform = host.platform;
  });

  $effect(() => setGameActive(route.name === "game"));
</script>

{#if fullscreen}
  {#if route.name === "launch"}
    <Launch gameId={route.gameId} />
  {:else if route.name === "game"}
    <Game gameId={route.gameId} gameView={host?.gameView ?? false} />
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
        <Install />
      {:else if route.name === "settings"}
        <Settings />
      {:else if route.name === "game-settings"}
        <GameSettings gameId={route.gameId} />
      {/if}
    </div>
  </div>
{/if}

<style>
  .shell {
    position: relative;
    height: 100%;
    display: flex;
    background: var(--bg);
    color: var(--fg);
    overflow: hidden;
  }
  .content {
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
</style>
