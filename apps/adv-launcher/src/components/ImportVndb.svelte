<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { slide } from "svelte/transition";
  import { t } from "../lib/i18n.svelte";
  import { call, inTauri } from "../lib/bridge";
  import type { Novel } from "../lib/host";
  import type { ImportPick } from "../lib/import-flow.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { setPref } from "../lib/settings-form.svelte";
  import ImportOption from "./ImportOption.svelte";
  import PhoneImportOption from "./PhoneImportOption.svelte";
  import VndbConsent from "./VndbConsent.svelte";

  let { pick, phone = false }: { pick: ImportPick; phone?: boolean } = $props();
  const Row = untrack(() => (phone ? PhoneImportOption : ImportOption));

  let query = $state(untrack(() => pick.title));
  let results = $state<Novel[]>([]);
  let more = $state(false);
  let page = 0;
  let activeQuery = "";
  let searched = $state(false);
  let busy = $state(false);
  let error = $state("");
  let asking = $state(false);
  /** Answers from an older search are dropped. */
  let request = 0;

  /** The chosen work stays listed even when a new search does not return it. */
  const rows = $derived(pick.vndb && !results.some((vn) => vn.id === pick.vndb!.id) ? [pick.vndb, ...results] : results);
  /** A search first lists its best few; the rest wait for "显示全部". */
  const FIRST = 5;
  let expanded = $state(false);
  /** After a pick only the choice stays, so the page below is in reach. */
  let collapsed = $state(false);
  const shown = $derived(collapsed ? rows.filter((vn) => vn.id === pick.vndb?.id) : expanded ? rows : rows.slice(0, FIRST));

  function pickNovel(vn: Novel | null): void {
    pick.setVndb(vn);
    collapsed = true;
  }

  async function search(next = false): Promise<void> {
    const text = next ? activeQuery : query.trim();
    if (!text || !inTauri) return;
    const id = ++request;
    const number = next ? page + 1 : 1;
    busy = true;
    error = "";
    collapsed = false;
    if (!next) expanded = false;
    try {
      const found = await call<{ results: Novel[]; more: boolean } | null>("vndb_search", { query: text, page: number }, null);
      if (id !== request || !found) return;
      results = next ? [...results, ...found.results] : found.results;
      more = found.more;
      page = number;
      activeQuery = text;
      searched = true;
    } catch (e) {
      if (id === request) error = String(e);
    } finally {
      if (id === request) busy = false;
    }
  }

  function choose(automatic: boolean): void {
    asking = false;
    setPref("fetchVndb", automatic);
    if (automatic) void search();
  }

  onMount(() => {
    if (!inTauri) return;
    if (!prefs.vndbAsked) asking = true;
    else if (prefs.fetchVndb) void search();
  });

  const year = (vn: Novel) => vn.released?.slice(0, 4);
</script>

<fieldset>
  <legend class="lbl">VNDB</legend>
  <form class="linkf" class:phone onsubmit={(e) => { e.preventDefault(); void search(); }}>
    <input type="search" lang="ja" aria-label={t("名称或 VNDB 链接")} placeholder={t("名称或 VNDB 链接")} bind:value={query} maxlength="200" />
    <button class="quiet" disabled={!query.trim() || !inTauri}>{t("搜索")}</button>
  </form>
  <div class="stack" aria-busy={busy}>
    {#if !collapsed || pick.vndb === null}
      <div class="item" transition:slide={{ duration: 180 }}>
        <Row name="vndb" checked={pick.vndb === null} onpick={() => pickNovel(null)}>
          <span class="name">{t("不关联")}</span>
        </Row>
      </div>
    {/if}
    {#each shown as vn (vn.id)}
      <div class="item" transition:slide={{ duration: 180 }}>
      <Row name="vndb" checked={pick.vndb?.id === vn.id} onpick={() => pickNovel(vn)}>
        <span class="thumb">
          {#if vn.image}<img src={vn.image.thumbnail} alt="" class:blur={prefs.blurAdult && vn.image.sexual >= 1} referrerpolicy="no-referrer" />{/if}
        </span>
        <span class="text">
          <span class="name" lang="ja">{vn.alttitle || vn.title}</span>
          <span class="meta" lang="ja">{[year(vn), vn.developers.map((d) => d.name).join(" / ")].filter(Boolean).join(" · ")}</span>
        </span>
      </Row>
      </div>
    {/each}
  </div>
  {#if collapsed && rows.length > 0}
    <button class="quiet more" onclick={() => (collapsed = false)}>{t("更换作品")}</button>
  {:else if busy}
    <div class="note" role="status">{t("正在搜索…")}</div>
  {:else if error}
    <div class="note err" role="alert">{error}</div>
  {:else if searched && results.length === 0}
    <div class="note" role="status">{t("没有找到作品")}</div>
  {:else if !expanded && rows.length > FIRST}
    <button class="quiet more" onclick={() => (expanded = true)}>{t("显示全部 {0} 个结果", rows.length)}</button>
  {:else if more}
    <button class="quiet more" onclick={() => void search(true)}>{t("加载更多")}</button>
  {/if}
  {#if pick.linkError}<div class="note err" role="alert">{pick.linkError}</div>{/if}
</fieldset>

{#if asking}<VndbConsent onchoose={choose} />{/if}

<style>
  fieldset {
    all: unset;
    display: block;
  }
  .lbl {
    margin: 0 0 12px;
    padding: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .linkf {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 44px;
    margin-bottom: 8px;
    padding: 0 6px 0 14px;
    border-radius: 10px;
    background: var(--field);
  }
  .linkf:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .linkf input {
    all: unset;
    flex: 1;
    min-width: 0;
    font-size: 13px;
    color: var(--fg);
  }
  .linkf.phone input {
    font-size: 14px;
  }
  .linkf input::placeholder {
    color: var(--fg3);
  }
  .linkf button:disabled {
    color: var(--fg3);
    cursor: default;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .thumb {
    flex: 0 0 auto;
    width: 32px;
    height: 44px;
    margin: 6px 0;
    border-radius: 3px;
    overflow: hidden;
    background: var(--field);
  }
  .thumb img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .thumb img.blur {
    filter: blur(6px);
    transform: scale(1.3);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .name {
    font-size: 14px;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    font-size: 12px;
    color: var(--fg3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .note {
    padding: 10px 16px 0;
    font-size: 13px;
    color: var(--fg3);
  }
  .note.err {
    color: var(--danger);
  }
  .more {
    margin: 6px 0 0 8px;
  }
</style>
