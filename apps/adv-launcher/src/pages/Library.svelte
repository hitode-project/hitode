<script lang="ts">
  import VndbLink from "../components/VndbLink.svelte";
  import { t } from "../lib/i18n.svelte";
  import Cover from "../components/Cover.svelte";
  import EmptyLibrary from "../components/EmptyLibrary.svelte";
  import Icon from "../components/Icon.svelte";
  import ManageMenu from "../components/ManageMenu.svelte";
  import ManageSaves from "../components/ManageSaves.svelte";
  import { bylineLabel, codepageLabel, lastPlayedLabel, playTimeLabel } from "../lib/format";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { canReveal, reveal } from "../lib/reveal";
  import type { Game, LibraryFilter } from "../lib/types";

  let { filter }: { filter: LibraryFilter } = $props();

  const titles: Record<LibraryFilter, string> = {
    all: t("全部游戏"),
    recent: t("最近游玩"),
    favorite: t("收藏"),
    unstarted: t("未开始"),
  };

  type Sort = "recent" | "title" | "brand" | "year";
  const sorts: { value: Sort; label: string }[] = [
    { value: "recent", label: t("最近游玩") },
    { value: "title", label: t("标题") },
    { value: "brand", label: t("品牌") },
    { value: "year", label: t("发行年份") },
  ];

  let query = $state("");
  let sort = $state<Sort>("recent");
  let selectedId = $state<string | null>(library.takeFocus());
  /** The detail panel is a sheet over the grid on narrow windows. */
  let sheet = $state(false);
  /** The "更多操作" menu or the saves dialog it opens. */
  let popup = $state<"none" | "menu" | "saves">("none");
  let moreButton = $state<HTMLButtonElement | null>(null);

  const items = $derived.by(() => {
    const list = library.filtered(filter, query);
    const collator = new Intl.Collator("ja");
    if (sort === "title") list.sort((a, b) => collator.compare(a.title, b.title));
    if (sort === "brand") list.sort((a, b) => collator.compare(a.brand, b.brand) || collator.compare(a.title, b.title));
    if (sort === "year") list.sort((a, b) => b.year - a.year);
    return list;
  });
  const current = $derived<Game | undefined>(items.find((g) => g.id === selectedId) ?? items[0]);

  function pick(game: Game) {
    selectedId = game.id;
    sheet = true;
    popup = "none";
  }

  function closeSheet() {
    sheet = false;
    popup = "none";
  }

  function closePopup() {
    popup = "none";
    moreButton?.focus();
  }

  function remove(game: Game) {
    const index = items.findIndex((g) => g.id === game.id);
    selectedId = (items[index + 1] ?? items[index - 1])?.id ?? null;
    void library.remove(game.id);
    popup = "none";
  }

  function offlineLabel(game: Game): string {
    return game.offline === "disk" ? t("未连接") : t("找不到");
  }

  function trend(game: Game) {
    return game.daily.map((minutes) => ({
      h: minutes ? Math.max(4, Math.round((minutes / 180) * 34)) : 2,
      on: minutes > 0,
    }));
  }

  /** A press outside the menu and its button closes the menu. */
  function onpointerdown(event: PointerEvent) {
    if (popup === "menu" && !(event.target instanceof Element && event.target.closest(".more"))) popup = "none";
  }

  function onkeydown(event: KeyboardEvent) {
    // The menu and the saves dialog handle their own Escape.
    if (event.key === "Escape" && popup === "none" && sheet) sheet = false;
  }
</script>

<svelte:window {onkeydown} onpointerdown={onpointerdown} />

{#if library.games.length === 0}
  <EmptyLibrary />
{:else}
  <header data-tauri-drag-region>
    <h1>{titles[filter]}</h1>
    <span class="count">{items.length}</span>
    <div style="flex: 1" data-tauri-drag-region></div>
    <label class="field search">
      <Icon name="search" size={15} stroke={1.5} />
      <span class="sr">{t("搜索")}</span>
      <input type="search" placeholder={t("搜索标题或品牌")} bind:value={query} />
    </label>
    <label class="pill sort">
      <span>{sorts.find((s) => s.value === sort)?.label}</span>
      <Icon name="chevron-down" size={14} stroke={1.5} />
      <select bind:value={sort} aria-label={t("排序")}>
        {#each sorts as s (s.value)}
          <option value={s.value}>{s.label}</option>
        {/each}
      </select>
    </label>
    <button class="icon-btn" aria-label={t("添加游戏")} onclick={() => nav.go({ name: "import" })}>
      <Icon name="plus" stroke={1.5} />
    </button>
  </header>

  <div class="body">
    <main class="scroll grid-wrap">
      {#if items.length === 0}
        <div class="none">{t("没有匹配的游戏")}</div>
      {/if}
      <div class="grid">
        {#each items as game (game.id)}
          <button
            class="card"
            class:on={current?.id === game.id}
            class:gone={game.offline !== null}
            onclick={() => pick(game)}
            ondblclick={() => game.offline === null && nav.go({ name: "launch", gameId: game.id })}
            aria-label={game.offline
              ? `${game.title}，${game.offline === "disk" ? t("磁盘未连接") : t("找不到文件夹")}`
              : game.title}
          >
            <Cover {game}>
              {#snippet children({ hidden })}
                {#if hidden}<span class="veil">{t("已遮挡")}</span>{/if}
                {#if game.offline}
                  <span class="badge" aria-hidden="true">
                    <Icon name="unlink" size={12} stroke={1.6} />
                    <span>{offlineLabel(game)}</span>
                  </span>
                {/if}
              {/snippet}
            </Cover>
            <div class="names">
              <span class="ttl" lang="ja">{game.title}</span>
              {#if game.brand}<span class="sub" lang="ja">{game.brand}</span>{/if}
            </div>
          </button>
        {/each}
      </div>
    </main>

    {#if sheet}
      <button class="shade" onclick={closeSheet} aria-label={t("关闭详情")}></button>
    {/if}
    {#if current}
      {@const game = current}
      <aside class="detail" class:open={sheet} aria-label={t("游戏详情")}>
        <div class="dbd {game.cover}" style:background-size="cover" style:background-position="center" style:background-image={game.coverUrl ? `url("${game.coverUrl}")` : undefined} aria-hidden="true"></div>
        <div class="dscrim" aria-hidden="true"></div>
        <div class="scroll detail-body">
          <button class="icon-btn d-close" onclick={closeSheet} aria-label={t("关闭详情")}>
            <Icon name="close" stroke={1.5} />
          </button>
          <Cover {game} style="width: 180px; flex: 0 0 auto; border-radius: 8px; box-shadow: 0 18px 40px rgba(0,0,0,.24), 0 0 0 1px var(--line)">
            {#snippet children({ hidden })}
              {#if hidden}
                <button class="reveal" onclick={() => (library.revealed[game.id] = true)} aria-label={t("显示封面")}>
                  <Icon name="eye" size={14} stroke={1.5} /><span>{t("显示")}</span>
                </button>
              {:else if game.nsfw && library.revealed[game.id]}
                <button class="rehide" onclick={() => (library.revealed[game.id] = false)} aria-label={t("重新遮挡封面")}>
                  <Icon name="eye-off" stroke={1.5} />
                </button>
              {/if}
            {/snippet}
          </Cover>
          <h2 lang="ja">{game.title}</h2>
          {#if bylineLabel(game.brand, game.year)}<div class="byline" lang="ja">{bylineLabel(game.brand, game.year)}</div>{/if}

          {#if game.offline}
            <div class="notice" role="status" style="margin-top: 22px">
              <Icon name={game.offline === "disk" ? "disk" : "folder-question"} />
              <div style="min-width: 0">
                <div style="color: var(--fg); font-weight: 500">
                  {game.offline === "disk" ? t("所在磁盘未连接") : t("找不到文件夹")}
                </div>
                <div class="mono" style="margin-top: 2px">{game.path}</div>
              </div>
            </div>
          {/if}

          <div class="actions" style="margin-top: {game.offline ? 12 : 22}px">
            {#if game.offline === "missing"}
              <button class="primary" onclick={() => void library.relocate(game.id)}>
                <Icon name="folder" size={15} stroke={1.5} />
                <span>{t("重新定位…")}</span>
              </button>
            {:else}
              <button
                class="primary"
                aria-disabled={game.offline === "disk" ? "true" : undefined}
                onclick={() => game.offline === null && nav.go({ name: "launch", gameId: game.id })}
              >
                <Icon name="play" size={14} />
                <span>{game.lastPlayed === null ? t("开始游戏") : t("继续游戏")}</span>
              </button>
            {/if}
            <div class="more">
              <button
                class="ghost"
                aria-label={t("更多操作")}
                aria-haspopup="menu"
                aria-expanded={popup === "menu"}
                bind:this={moreButton}
                onclick={() => (popup = popup === "menu" ? "none" : "menu")}
              >
                <Icon name="more" />
              </button>
              {#if popup === "menu"}
                <ManageMenu {game} onclose={closePopup} onsaves={() => (popup = "saves")} onremove={() => remove(game)} />
              {/if}
            </div>
          </div>
          {#if game.offline === "missing"}
            <button class="quiet remove" onclick={() => remove(game)}>{t("从游戏库移除")}</button>
          {/if}

          <div class="rule"></div>
          <dl class="meta">
            <dt>{t("上次游玩")}</dt><dd>{lastPlayedLabel(game.lastPlayed, library.now)}</dd>
            <dt>{t("累计时长")}</dt><dd>{playTimeLabel(game.playMinutes)}</dd>
            {#if game.engine}<dt>{t("引擎")}</dt><dd>{game.engine}</dd>{/if}
            <dt>VNDB</dt><dd><VndbLink {game} /></dd>
            <dt>{t("编码")}</dt><dd>{codepageLabel(game.codepage)}</dd>
          </dl>
          {#if canReveal()}
            <button class="quiet locate" title={game.path} disabled={game.offline !== null} onclick={() => reveal(game.path)}>
              <Icon name="folder" size={15} />
              <span>{t("在访达中显示")}</span>
            </button>
          {/if}
          {#if game.lastPlayed !== null}
            <div style="margin-top: 24px">
              <div class="trend-head"><span>{t("近 30 天")}</span><span>{game.sessions30} {t("次")}</span></div>
              <div class="trend" role="img" aria-label={t("近 30 天每日游玩时长")}>
                {#each trend(game) as day, i (i)}
                  <span class="tb" class:on={day.on} style="height: {day.h}px"></span>
                {/each}
              </div>
            </div>
          {/if}
        </div>
      </aside>
      {#if popup === "saves"}
        <ManageSaves {game} onclose={closePopup} />
      {/if}
    {/if}
  </div>
{/if}

<style>
  header {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px 24px 14px 32px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    letter-spacing: 0.01em;
  }
  .count {
    font-size: 13px;
    color: var(--fg3);
  }
  .search {
    width: 240px;
  }
  .sort {
    position: relative;
  }
  .sort select {
    position: absolute;
    inset: 0;
    opacity: 0;
    cursor: pointer;
  }
  .body {
    position: relative;
    flex: 1 1 0;
    min-height: 0;
    display: flex;
  }
  .grid-wrap {
    flex: 1 1 0;
    min-width: 0;
    overflow: auto;
    /* Keep the scrollbar's room even when nothing scrolls, so the grid does
       not change scale between a long list and a short one. */
    scrollbar-gutter: stable;
    padding: 10px 32px 48px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(132px, 1fr));
    column-gap: 24px;
    row-gap: 28px;
  }
  .none {
    padding: 48px 0;
    text-align: center;
    font-size: 13px;
    color: var(--fg3);
  }
  .card {
    all: unset;
    display: flex;
    flex-direction: column;
    gap: 9px;
    cursor: pointer;
    min-width: 0;
  }
  .card:hover :global(.cv) {
    transform: translateY(-2px);
    box-shadow: 0 0 0 1px var(--line), 0 8px 20px rgba(0, 0, 0, 0.12);
  }
  .card.on :global(.cv) {
    box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--accent);
  }
  .card:focus-visible :global(.cv) {
    outline: 2px solid var(--accent);
    outline-offset: 4px;
  }
  .names {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .ttl {
    font-size: 13px;
    line-height: 18px;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .card.gone .ttl {
    color: var(--fg3);
  }
  .sub {
    font-size: 12px;
    line-height: 16px;
    color: var(--fg3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .veil {
    position: absolute;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 4px 9px;
    border-radius: 999px;
    background: rgba(20, 20, 20, 0.55);
    color: #fff;
    font-size: 11px;
    white-space: nowrap;
    backdrop-filter: blur(6px);
  }
  .badge {
    position: absolute;
    left: 6px;
    bottom: 6px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 3px 8px 3px 6px;
    border-radius: 999px;
    background: rgba(20, 20, 20, 0.66);
    color: #fff;
    font-size: 11px;
    line-height: 14px;
    white-space: nowrap;
    backdrop-filter: blur(6px);
  }

  .detail {
    position: relative;
    flex: 0 0 340px;
    overflow: hidden;
    isolation: isolate;
    contain: paint;
    background: var(--panel);
    border-left: 1px solid var(--line);
  }
  .dbd {
    position: absolute;
    inset: -25%;
    filter: blur(60px) saturate(1.2);
    opacity: 0.75;
    /* Its own layer, so WebKit does not re-rasterize the blur when a hovered card recomposites. */
    transform: translateZ(0);
    will-change: transform;
  }
  .dscrim {
    position: absolute;
    inset: 0;
    background: linear-gradient(180deg, rgba(249, 249, 247, 0.2) 0%, rgba(249, 249, 247, 0.7) 40%, #f9f9f7 64%);
  }
  :global([data-theme="dark"]) .dscrim {
    background: linear-gradient(180deg, rgba(24, 24, 26, 0.15) 0%, rgba(24, 24, 26, 0.7) 40%, #18181a 64%);
  }
  .detail-body {
    position: relative;
    height: 100%;
    box-sizing: border-box;
    overflow: auto;
    padding: 32px 28px 28px;
    display: flex;
    flex-direction: column;
  }
  .d-close {
    display: none;
    position: absolute;
    top: 14px;
    right: 14px;
    width: 40px;
    height: 40px;
    border-radius: 20px;
  }
  .detail h2 {
    margin: 24px 0 6px;
    font-size: 22px;
    line-height: 1.35;
    font-weight: 600;
    text-wrap: pretty;
  }
  .byline {
    font-size: 13px;
    color: var(--fg2);
  }
  .reveal {
    all: unset;
    box-sizing: border-box;
    position: absolute;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    display: flex;
    align-items: center;
    gap: 6px;
    height: 36px;
    padding: 0 14px;
    border-radius: 18px;
    background: rgba(20, 20, 20, 0.6);
    color: #fff;
    font-size: 13px;
    white-space: nowrap;
    cursor: pointer;
    backdrop-filter: blur(6px);
  }
  .reveal:hover {
    background: rgba(20, 20, 20, 0.75);
  }
  .rehide {
    all: unset;
    box-sizing: border-box;
    position: absolute;
    right: 8px;
    top: 8px;
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
    border-radius: 16px;
    background: rgba(20, 20, 20, 0.5);
    color: #fff;
    cursor: pointer;
    backdrop-filter: blur(6px);
  }
  .rehide:hover {
    background: rgba(20, 20, 20, 0.7);
  }
  .reveal:focus-visible,
  .rehide:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .actions .primary {
    flex: 1;
  }
  .more {
    position: relative;
  }
  .remove {
    align-self: flex-start;
    margin: 12px 0 0 -8px;
  }
  .locate {
    display: flex;
    align-items: center;
    gap: 6px;
    align-self: flex-start;
    margin: 10px 0 0 -8px;
  }
  .locate[disabled] {
    opacity: 0.4;
    cursor: default;
    background: none;
  }
  .rule {
    height: 1px;
    background: var(--line);
    margin: 26px 0 20px;
  }
  .meta {
    display: grid;
    grid-template-columns: auto 1fr;
    column-gap: 20px;
    row-gap: 12px;
    margin: 0;
    font-size: 13px;
  }
  .meta dt {
    color: var(--fg3);
  }
  .meta dd {
    margin: 0;
    color: var(--fg);
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .trend-head {
    display: flex;
    justify-content: space-between;
    font-size: 12px;
    color: var(--fg3);
    margin-bottom: 8px;
  }
  .trend {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    height: 36px;
    border-bottom: 1px solid var(--line);
  }
  .tb {
    flex: 1;
    min-width: 0;
    border-radius: 2px 2px 0 0;
    background: var(--line);
  }
  .tb.on {
    background: var(--chart);
  }
  .shade {
    all: unset;
    display: none;
    position: absolute;
    inset: 0;
    z-index: 2;
    background: rgba(0, 0, 0, 0.28);
    cursor: pointer;
  }
  @media (max-width: 1100px) {
    .detail {
      position: absolute;
      top: 0;
      right: 0;
      bottom: 0;
      width: min(360px, 100%);
      z-index: 3;
      box-shadow: -16px 0 40px rgba(0, 0, 0, 0.18);
      transform: translateX(100%);
      visibility: hidden;
      transition: transform 0.22s cubic-bezier(0.2, 0.8, 0.2, 1), visibility 0.22s;
    }
    .detail.open {
      transform: none;
      visibility: visible;
    }
    .shade {
      display: block;
    }
    .d-close {
      display: grid;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .detail {
      transition: none;
    }
  }
</style>
