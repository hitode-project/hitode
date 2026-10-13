<script lang="ts">
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import PhoneManageSheet from "../components/PhoneManageSheet.svelte";
  import { lastPlayedLabel, playTimeLabel } from "../lib/format";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import type { Game, LibraryFilter } from "../lib/types";

  let { filter }: { filter: LibraryFilter } = $props();

  const chips: { filter: LibraryFilter; label: string }[] = [
    { filter: "all", label: "全部" },
    { filter: "recent", label: "最近" },
    { filter: "favorite", label: "收藏" },
    { filter: "unstarted", label: "未开始" },
  ];

  let searching = $state(false);
  let query = $state("");
  let searchInput = $state<HTMLInputElement | null>(null);
  /** The game whose detail page is open. */
  let openId = $state<string | null>(null);
  let more = $state(false);
  /** "已从游戏库移除" with undo, shown for a few seconds after a removal. */
  let undoToast = $state(false);

  $effect(() => {
    if (!undoToast) return;
    const timer = setTimeout(() => (undoToast = false), 5000);
    return () => clearTimeout(timer);
  });

  const items = $derived(library.filtered(filter, searching ? query : ""));
  const current = $derived<Game | undefined>(openId ? library.get(openId) : undefined);

  function toggleSearch() {
    searching = !searching;
    query = "";
    if (searching) queueMicrotask(() => searchInput?.focus());
  }

  function offlineLabel(game: Game): string {
    return game.offline === "disk" ? "未连接" : "找不到";
  }

  function trend(game: Game) {
    return game.daily.map((minutes) => ({
      h: minutes ? Math.max(4, Math.round((minutes / 180) * 38)) : 2,
      on: minutes > 0,
    }));
  }

  function remove(game: Game) {
    library.remove(game.id);
    removed();
  }

  function removed() {
    openId = null;
    more = false;
    undoToast = true;
  }

  // The back gesture closes the detail page, then the search field; the
  // manage sheet registers its own handler while it is open.
  $effect(() => {
    if (!current && !searching) return;
    return nav.onBack(() => {
      if (current) openId = null;
      else searching = false;
      return true;
    });
  });
</script>

{#if library.games.length === 0}
  <header class="top">
    <h1>游戏库</h1>
    <button class="icon-btn" aria-label="设置" onclick={() => nav.go({ name: "settings" })}>
      <Icon name="sliders" size={20} stroke={1.5} />
    </button>
  </header>
  <main class="empty">
    <div aria-hidden="true" class="stack">
      <div class="ghost-card" style="transform: rotate(-6deg) translateY(6px); opacity: .6"></div>
      <div class="ghost-card" style="width: 76px"></div>
      <div class="ghost-card" style="transform: rotate(6deg) translateY(6px); opacity: .6"></div>
    </div>
    <h2>添加游戏文件夹</h2>
    <div class="empty-actions">
      <button class="primary" onclick={() => nav.go({ name: "scan" })}>
        <Icon name="folder-plus" size={18} stroke={1.5} />
        <span>添加文件夹…</span>
      </button>
      <button class="quiet">支持哪些游戏？</button>
    </div>
  </main>
{:else}
  <header class="top">
    <h1>游戏库</h1>
    <button class="icon-btn" aria-label="搜索" aria-pressed={searching} onclick={toggleSearch}>
      <Icon name="search" size={20} stroke={1.5} />
    </button>
    <button class="icon-btn" aria-label="游玩统计" onclick={() => nav.go({ name: "stats" })}>
      <Icon name="chart" size={20} stroke={1.5} />
    </button>
    <button class="icon-btn" aria-label="添加游戏" onclick={() => nav.go({ name: "scan" })}>
      <Icon name="plus" size={20} stroke={1.5} />
    </button>
    <button class="icon-btn" aria-label="设置" onclick={() => nav.go({ name: "settings" })}>
      <Icon name="sliders" size={20} stroke={1.5} />
    </button>
  </header>

  {#if searching}
    <label class="field search">
      <Icon name="search" size={16} stroke={1.5} />
      <span class="sr">搜索</span>
      <input type="search" placeholder="搜索标题或品牌" bind:this={searchInput} bind:value={query} />
    </label>
  {:else}
    <div class="chips" role="group" aria-label="分类">
      {#each chips as c (c.filter)}
        <button
          class="chip"
          class:on={filter === c.filter}
          aria-pressed={filter === c.filter}
          onclick={() => nav.replace({ name: "library", filter: c.filter })}
        >
          {c.label}{filter === c.filter ? ` ${items.length}` : ""}
        </button>
      {/each}
    </div>
  {/if}

  <main class="grid-wrap">
    {#if items.length === 0}
      <div class="none">没有匹配的游戏</div>
    {/if}
    <div class="grid">
      {#each items as game (game.id)}
        <button
          class="card"
          class:gone={game.offline !== null}
          onclick={() => (openId = game.id)}
          aria-label={game.offline
            ? `${game.title}，${game.offline === "disk" ? "磁盘未连接" : "找不到文件夹"}`
            : game.title}
        >
          <Cover {game} style="border-radius: 5px">
            {#snippet children({ hidden })}
              {#if hidden}<span class="veil">已遮挡</span>{/if}
              {#if game.offline}
                <span class="badge" aria-hidden="true">
                  <Icon name="unlink" size={10} stroke={1.6} />
                  <span>{offlineLabel(game)}</span>
                </span>
              {/if}
            {/snippet}
          </Cover>
          <span class="ttl" lang="ja">{game.title}</span>
        </button>
      {/each}
    </div>
  </main>
{/if}

{#if current}
  {@const game = current}
  <div class="page" role="dialog" aria-label="游戏详情">
    <div class="pbd {game.cover}" aria-hidden="true"></div>
    <div class="pscrim" aria-hidden="true"></div>
    <div class="page-body">
      <div style="display: flex; margin-left: -12px">
        <button class="icon-btn" onclick={() => (openId = null)} aria-label="返回游戏库">
          <Icon name="chevron-left" size={22} stroke={1.6} />
        </button>
      </div>
      <div class="hero">
        <Cover {game} style="width: 160px; border-radius: 8px; box-shadow: 0 20px 44px rgba(0,0,0,.28), 0 0 0 1px var(--line)">
          {#snippet children({ hidden })}
            {#if hidden}
              <button class="reveal" onclick={() => (library.revealed[game.id] = true)} aria-label="显示封面">
                <Icon name="eye" size={14} stroke={1.5} /><span>显示</span>
              </button>
            {:else if game.nsfw && library.revealed[game.id]}
              <button class="rehide" onclick={() => (library.revealed[game.id] = false)} aria-label="重新遮挡封面">
                <Icon name="eye-off" stroke={1.5} />
              </button>
            {/if}
          {/snippet}
        </Cover>
        <h2 lang="ja">{game.title}</h2>
        <div class="byline"><span lang="ja">{game.brand}</span> · {game.year}</div>
      </div>

      {#if game.offline}
        <div class="notice phone-notice" role="status">
          <Icon name={game.offline === "disk" ? "disk" : "folder-question"} />
          <div style="min-width: 0">
            <div style="font-weight: 500">{game.offline === "disk" ? "所在磁盘未连接" : "找不到文件夹"}</div>
            <div class="mono" style="margin-top: 2px">{game.path}</div>
          </div>
        </div>
      {/if}

      <div class="actions" style="margin-top: {game.offline ? 12 : 26}px">
        {#if game.offline === "missing"}
          <button class="primary" onclick={() => library.relocate(game.id)}>
            <Icon name="folder" size={16} stroke={1.5} />
            <span>重新定位…</span>
          </button>
        {:else}
          <button
            class="primary"
            aria-disabled={game.offline === "disk" ? "true" : undefined}
            onclick={() => game.offline === null && nav.go({ name: "launch", gameId: game.id })}
          >
            <Icon name="play" size={15} />
            <span>{game.lastPlayed === null ? "开始游戏" : "继续游戏"}</span>
          </button>
        {/if}
        <button class="ghost" aria-label="更多操作" aria-haspopup="dialog" onclick={() => (more = true)}>
          <Icon name="more" size={18} />
        </button>
      </div>
      {#if game.offline === "missing"}
        <button class="quiet remove" onclick={() => remove(game)}>从游戏库移除</button>
      {/if}

      <dl class="meta">
        <dt>上次游玩</dt><dd>{lastPlayedLabel(game.lastPlayed, library.now)}</dd>
        <dt>累计时长</dt><dd>{playTimeLabel(game.playMinutes)}</dd>
      </dl>
      {#if game.lastPlayed !== null}
        <div style="margin-top: 22px">
          <div class="trend-head"><span>近 30 天</span><span>{game.sessions30} 次</span></div>
          <div class="trend" role="img" aria-label="近 30 天每日游玩时长">
            {#each trend(game) as day, i (i)}
              <span class="tb" class:on={day.on} style="height: {day.h}px"></span>
            {/each}
          </div>
        </div>
      {/if}
    </div>
  </div>
{/if}

{#if current && more}
  <PhoneManageSheet game={current} onclose={() => (more = false)} onremoved={removed} />
{/if}

{#if undoToast}
  <div class="toast" role="status">
    <span style="flex: 1">已从游戏库移除</span>
    <button onclick={() => { library.undoRemove(); undoToast = false; }}>撤销</button>
  </div>
{/if}

<style>
  .top {
    flex: 0 0 auto;
    padding: calc(var(--safe-top) + 12px) 8px 0 20px;
    display: flex;
    align-items: center;
  }
  h1 {
    margin: 0;
    flex: 1;
    font-size: 30px;
    line-height: 40px;
    font-weight: 700;
    letter-spacing: 0.01em;
  }
  .icon-btn {
    width: 44px;
    height: 44px;
    border-radius: 22px;
    color: var(--fg);
  }
  .icon-btn[aria-pressed="true"] {
    background: var(--field);
  }
  .chips {
    flex: 0 0 auto;
    display: flex;
    gap: 8px;
    padding: 14px 20px 18px;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .chip {
    all: unset;
    box-sizing: border-box;
    height: 32px;
    padding: 0 14px;
    display: flex;
    align-items: center;
    border-radius: 16px;
    background: var(--field);
    color: var(--fg2);
    font-size: 13px;
    white-space: nowrap;
    cursor: pointer;
  }
  .chip.on {
    background: var(--fg);
    color: var(--bg);
    font-weight: 500;
  }
  .chip:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .search {
    flex: 0 0 auto;
    margin: 12px 20px 16px;
    height: 40px;
    border-radius: 12px;
  }
  .search input {
    font-size: 15px;
  }
  .grid-wrap {
    flex: 1 1 0;
    min-height: 0;
    overflow-y: auto;
    padding: 0 20px calc(var(--safe-bottom) + 24px);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    column-gap: 12px;
    row-gap: 18px;
  }
  .none {
    padding: 48px 0;
    text-align: center;
    font-size: 14px;
    color: var(--fg3);
  }
  .card {
    all: unset;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    cursor: pointer;
  }
  .card:focus-visible :global(.cv) {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }
  .ttl {
    font-size: 12px;
    line-height: 16px;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .card.gone .ttl {
    color: var(--fg3);
  }
  .veil {
    position: absolute;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    padding: 3px 8px;
    border-radius: 999px;
    background: rgba(20, 20, 20, 0.55);
    color: #fff;
    font-size: 10px;
    white-space: nowrap;
  }
  .badge {
    position: absolute;
    left: 5px;
    bottom: 5px;
    display: flex;
    align-items: center;
    gap: 3px;
    padding: 2px 7px 2px 5px;
    border-radius: 999px;
    background: rgba(20, 20, 20, 0.66);
    color: #fff;
    font-size: 10px;
    line-height: 14px;
    white-space: nowrap;
  }

  .empty {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    padding: 0 24px calc(var(--safe-bottom) + 96px);
  }
  .stack {
    display: flex;
    align-items: flex-end;
    gap: 10px;
    margin-bottom: 32px;
  }
  .ghost-card {
    width: 64px;
    aspect-ratio: 5 / 7;
    border-radius: 7px;
    background: var(--field);
    border: 1px solid var(--line);
  }
  .empty h2 {
    margin: 0;
    font-size: 24px;
    line-height: 1.3;
    font-weight: 600;
  }
  .empty-actions {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    margin-top: 28px;
  }

  .page {
    position: absolute;
    inset: 0;
    z-index: 5;
    background: var(--bg);
    overflow: hidden;
    animation: in 0.26s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  @keyframes in {
    from {
      transform: translateX(24px);
      opacity: 0;
    }
    to {
      transform: none;
      opacity: 1;
    }
  }
  .pbd {
    position: absolute;
    inset: -15% -30% auto -30%;
    height: 560px;
    filter: blur(60px) saturate(1.2);
    opacity: 0.8;
  }
  .pscrim {
    position: absolute;
    inset: 0;
    background: linear-gradient(180deg, rgba(244, 244, 242, 0.2) 0%, rgba(244, 244, 242, 0.7) 34%, #f4f4f2 58%);
  }
  :global([data-theme="dark"]) .pscrim {
    background: linear-gradient(180deg, rgba(20, 20, 21, 0.15) 0%, rgba(20, 20, 21, 0.7) 34%, #141415 58%);
  }
  .page-body {
    position: relative;
    height: 100%;
    box-sizing: border-box;
    overflow-y: auto;
    padding: calc(var(--safe-top) + 6px) 20px calc(var(--safe-bottom) + 24px);
    display: flex;
    flex-direction: column;
  }
  .hero {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    margin-top: 6px;
  }
  .hero h2 {
    margin: 22px 0 6px;
    font-size: 22px;
    line-height: 1.35;
    font-weight: 600;
    text-wrap: balance;
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
  .reveal:focus-visible,
  .rehide:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .phone-notice {
    margin-top: 22px;
    border-radius: 14px;
    font-size: 14px;
    line-height: 20px;
  }
  .actions {
    display: flex;
    gap: 10px;
  }
  .primary {
    height: 48px;
    border-radius: 24px;
    font-size: 15px;
  }
  .actions .primary {
    flex: 1;
  }
  .ghost {
    width: 48px;
    height: 48px;
    border-radius: 24px;
  }
  .quiet {
    height: 44px;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    padding: 0 14px;
    border-radius: 22px;
    font-size: 14px;
  }
  .remove {
    align-self: center;
    margin-top: 10px;
  }
  .meta {
    display: grid;
    grid-template-columns: auto 1fr;
    column-gap: 20px;
    row-gap: 12px;
    margin: 26px 0 0;
    font-size: 14px;
  }
  .meta dt {
    color: var(--fg3);
  }
  .meta dd {
    margin: 0;
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
    height: 40px;
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
  .toast {
    position: absolute;
    left: 16px;
    right: 16px;
    bottom: calc(var(--safe-bottom) + 8px);
    z-index: 7;
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 52px;
    padding: 0 6px 0 18px;
    border-radius: 16px;
    background: var(--fg);
    color: var(--bg);
    font-size: 14px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.25);
    animation: fade 0.2s ease-out;
  }
  .toast button {
    all: unset;
    box-sizing: border-box;
    height: 44px;
    padding: 0 14px;
    display: flex;
    align-items: center;
    border-radius: 22px;
    font-weight: 500;
    cursor: pointer;
  }
  .toast button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
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
    .page,
    .toast {
      animation: none;
    }
  }
</style>
