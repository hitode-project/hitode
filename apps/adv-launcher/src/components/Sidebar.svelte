<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "./Icon.svelte";
  import type { IconName } from "../lib/icons";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import type { LibraryFilter } from "../lib/types";

  const filters: { filter: LibraryFilter; label: string; icon: IconName }[] = [
    { filter: "all", label: t("全部游戏"), icon: "grid" },
    { filter: "recent", label: t("最近游玩"), icon: "clock" },
    { filter: "favorite", label: t("收藏"), icon: "star" },
    { filter: "unstarted", label: t("未开始"), icon: "unstarted" },
  ];

  const route = $derived(nav.route);
  const adding = $derived(route.name === "import" || route.name === "scan" || route.name === "install");
</script>

<nav class="side" aria-label={t("分类")}>
  <div class="drag" data-tauri-drag-region></div>
  <div class="navlabel">{t("游戏库")}</div>
  {#each filters as f (f.filter)}
    <button
      class="nav"
      class:on={route.name === "library" && route.filter === f.filter}
      aria-current={route.name === "library" && route.filter === f.filter ? "page" : undefined}
      onclick={() => nav.home(f.filter)}
    >
      <Icon name={f.icon} />
      <span>{f.label}</span><span class="n">{library.count(f.filter)}</span>
    </button>
  {/each}
  <div class="navlabel">{t("记录")}</div>
  <button class="nav" class:on={route.name === "stats"} onclick={() => nav.go({ name: "stats" })}>
    <Icon name="chart" /><span>{t("游玩统计")}</span>
  </button>
  <div style="flex: 1"></div>
  <button class="nav" class:on={adding} onclick={() => nav.go({ name: "scan" })}>
    <Icon name="folder-plus" /><span>{t("添加文件夹")}</span>
  </button>
  <button class="nav" class:on={route.name === "settings"} onclick={() => nav.go({ name: "settings" })}>
    <Icon name="sliders" /><span>{t("设置")}</span>
  </button>
</nav>

<style>
  .side {
    position: relative;
    flex: 0 0 220px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 44px 12px 16px;
    background: var(--side);
    border-right: 1px solid var(--line);
  }
  /* Room for the macOS traffic lights; the window is dragged from here. */
  .drag {
    position: absolute;
    inset: 0 0 auto 0;
    height: 40px;
  }
  @media (max-width: 760px) {
    .side {
      display: none;
    }
  }
</style>
