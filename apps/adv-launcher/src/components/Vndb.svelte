<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { t } from "../lib/i18n.svelte";
  import { call, inTauri } from "../lib/bridge";
  import { library } from "../lib/library.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import type { LibraryGame, Novel } from "../lib/host";
  import type { Game } from "../lib/types";
  let { game, onclose }: { game: Game; onclose: () => void } = $props();
  let searchInput: HTMLInputElement;
  onMount(() => searchInput.focus());
  let query = $state(untrack(() => game.title));
  let title = $state(untrack(() => game.title));
  let selected = $state<Novel | null>(untrack(() => game.vndb ?? null));
  let results = $state<Novel[]>([]);
  let more = $state(false);
  let page = $state(0);
  let searched = $state(false);
  let activeQuery = "";
  let busy = $state(false);
  let saving = $state(false);
  let error = $state("");
  async function search(next = false) {
    if (!query.trim() || busy) return;
    busy = true; error = "";
    const text = next ? activeQuery : query.trim();
    const number = next ? page + 1 : 1;
    if (!next) { results = []; selected = null; more = false; searched = false; }
    try {
      const found = await call<{ results: Novel[]; more: boolean } | null>("vndb_search", { query: text, page: number }, null);
      if (!found) throw Error(t("请在桌面启动器中使用 VNDB"));
      results = next ? [...results, ...found.results] : found.results;
      more = found.more; page = number; activeQuery = text; searched = true;
      if (!next) selected = found.results.length === 1 ? found.results[0]! : null;
    } catch (e) { error = String(e); }
    finally { busy = false; saving = false; }
  }
  async function save() {
    if (!selected || busy || !title.trim()) return;
    busy = true; saving = true; error = "";
    try {
      const entry = await call<LibraryGame | null>("vndb_apply", { id: game.id, vn: selected.id, title: title.trim() }, null);
      if (!entry) throw Error(t("请在桌面启动器中使用 VNDB"));
      library.put(entry); onclose();
    } catch (e) { error = String(e); }
    finally { busy = false; saving = false; }
  }
  async function unlink() {
    busy = true; saving = true; error = "";
    try {
      const entry = await call<LibraryGame | null>("vndb_unlink", { id: game.id }, null);
      if (entry) { library.put(entry); onclose(); }
    } catch (e) { error = String(e); }
    finally { busy = false; saving = false; }
  }
</script>
<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-label="VNDB" tabindex="-1" onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); if (!saving) onclose(); } }}>
    <header><h2>{t("VNDB 资料")}</h2><button class="quiet" onclick={onclose} disabled={saving}>{t("关闭")}</button></header>
    <form onsubmit={(e) => { e.preventDefault(); void search(); }}>
      <input bind:this={searchInput} aria-label={t("名称或 VNDB 链接")} placeholder={t("名称或 VNDB 链接")} bind:value={query} maxlength="200" disabled={busy} />
      <button class="primary" disabled={busy || !query.trim() || !inTauri}>{t("搜索")}</button>
    </form>
    {#if game.vndb}<p><a href="https://vndb.org/{game.vndb.id}" target="_blank" rel="noreferrer">VNDB · {game.vndb.id}</a> <button class="quiet" disabled={busy} onclick={() => { query = game.vndb!.id; void search(); }}>{t("刷新当前资料")}</button> <button class="quiet" disabled={busy} onclick={unlink}>{t("解除关联")}</button></p>{/if}
    {#if busy}<p role="status">{t("正在获取资料…")}</p>{/if}
    {#if error}<p role="alert">{error}</p>{/if}
    <div class="results">
      {#each results as vn (vn.id)}
        <button class="result" class:on={selected?.id === vn.id} disabled={busy} onclick={() => selected = vn} aria-pressed={selected?.id === vn.id}>
          {#if vn.image}<img src={vn.image.thumbnail} alt="" class:blur={prefs.blurAdult && vn.image.sexual >= 1} referrerpolicy="no-referrer" />{/if}
          <span><strong>{vn.alttitle || vn.title}</strong><br />{vn.title}<br /><small>{vn.id} · {vn.released ?? "—"} · {vn.developers.map((d) => d.name).join(" / ")}</small></span>
        </button>
      {/each}
    </div>
    {#if searched && results.length === 0}<p>{t("没有找到作品")}</p>{/if}
    {#if more}<button class="quiet" disabled={busy} onclick={() => search(true)}>{t("加载更多")}</button>{/if}
    {#if selected}
      <section>
        <h3>{selected.alttitle || selected.title}</h3>
        <a href="https://vndb.org/{selected.id}" target="_blank" rel="noreferrer">https://vndb.org/{selected.id}</a>
        <p class="description">{selected.description ?? ""}</p>
        <label>{t("游戏库显示名称")}<input bind:value={title} disabled={busy} /></label>
        <button class="quiet" disabled={busy} onclick={() => title = selected!.alttitle || selected!.title}>{t("使用作品原名")}</button>
        <button class="primary" disabled={busy || !title.trim()} onclick={save}>{t("应用资料")}</button>
      </section>
    {/if}
  </div>
</div>
<style>
  .backdrop { position: fixed; inset: 0; z-index: 100; display: grid; place-items: center; padding: 20px; background: #0006; }
  .dialog { width: min(720px, 100%); max-height: 85vh; overflow: auto; background: var(--pop); color: var(--fg); padding: 24px; border-radius: 16px; box-sizing: border-box; }
  header, form { display: flex; align-items: center; gap: 12px; }
  header { justify-content: space-between; }
  input { box-sizing: border-box; min-width: 0; flex: 1; padding: 10px; border: 1px solid var(--line); border-radius: 6px; background: var(--field); color: var(--fg); }
  label { display: grid; gap: 8px; }
  p { line-height: 1.6; }
  .results { display: grid; gap: 8px; }
  .result { text-align: left; display: flex; gap: 16px; padding: 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--field); color: var(--fg); cursor: pointer; }
  .result.on { border-color: var(--accent); }
  img { width: 64px; height: 90px; object-fit: cover; }
  .blur { filter: blur(12px); }
  .description { white-space: pre-wrap; max-height: 160px; overflow: auto; }
</style>
