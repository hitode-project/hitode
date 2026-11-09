<script lang="ts">
  import Cover from "../components/Cover.svelte";
  import Icon from "../components/Icon.svelte";
  import PhoneSettingsHeader from "../components/PhoneSettingsHeader.svelte";
  import { preview } from "../lib/bridge";
  import { storageSize } from "../lib/storage";
  import { device } from "../lib/device.svelte";
  import { t } from "../lib/i18n.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";
  import { gameStorage, type GameCopy, type GameLocation } from "../lib/game-storage.svelte";

  let { gameId }: { gameId: string | undefined } = $props();
  let expanded = $state<string | null>(null);
  let operation = $state<"delete" | "copy" | "copied" | "open">("open");
  let active = $state<GameCopy>();
  let path = $state("");
  let dialog: HTMLDialogElement;
  let dialogOpen = $state(false);
  const game = $derived(gameId ? library.get(gameId) : undefined);
  let location = $state<GameLocation | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let problem = $state("");
  let exportedPath = $state<string | null>(null);
  const locationLabel = $derived(game?.offline === "disk" ? "所在磁盘未连接"
    : game?.offline ? location?.kind === "copy" ? "文件已移除" : "找不到文件夹"
    : location?.kind === "copy" ? "Hitode 内部副本" : location?.kind === "documents" ? "Hitode 文件夹" : "外部文件夹");
  const gameFolder = $derived(game?.offline ? null : location?.copy);
  const displayCopy = (folder: GameCopy) => preview ? folder.directory : "Hitode/imports/" + folder.id;

  $effect(() => {
    const id = gameId;
    void game?.path;
    void game?.offline;
    let current = true;
    loading = true;
    problem = "";
    location = null;
    const load = id ? gameStorage.location(id) : gameStorage.refresh();
    void load.then((value) => {
      if (current) location = value ?? null;
    }).catch((error) => {
      if (current) problem = String(error);
    }).finally(() => {
      if (current) loading = false;
    });
    return () => { current = false; };
  });

  $effect(() => {
    if (busy || dialogOpen) {
      return nav.onBack(() => {
        if (!busy && dialogOpen) dialog.close();
        return true;
      });
    }
  });

  function show(next: typeof operation, location: string, folder?: GameCopy) {
    active = folder;
    path = location;
    operation = next;
    dialog.showModal();
    dialogOpen = true;
  }

  async function run(work: () => Promise<void>) {
    if (busy) return;
    busy = true;
    problem = "";
    try {
      await work();
    } catch (error) {
      problem = String(error);
    } finally {
      busy = false;
    }
  }

  function reveal(label: string, folder?: GameCopy) {
    if (preview) show("open", label);
    else void run(() => gameStorage.reveal(gameId ?? null, folder));
  }

  function exportCopy(folder: GameCopy) {
    if (preview) {
      exportedPath = "~/Downloads/" + folder.id;
      show("copy", exportedPath, folder);
    } else {
      void run(async () => {
        const exported = await gameStorage.export(folder);
        if (!exported) return;
        exportedPath = exported.path;
        show("copied", exported.label, folder);
      });
    }
  }

  function deleteCopy(folder: GameCopy) {
    void run(async () => {
      await gameStorage.delete(folder);
      dialog.close();
      if (expanded === folder.id) expanded = null;
    });
  }

  function openGame(id: string) {
    nav.go({ name: "game-files", gameId: id });
  }
</script>

{#snippet actions(label: string, folder?: GameCopy | null, canReveal = true)}
  <div class="actions">
    {#if canReveal}<button class="primary" disabled={busy} onclick={() => reveal(label, folder ?? undefined)}>{t("打开所在文件夹")}</button>{/if}
    {#if folder}
      <button class={canReveal ? "secondary" : "primary"} disabled={busy} onclick={() => exportCopy(folder)}>{t("复制到其他位置…")}</button>
      <button class="quiet destructive" disabled={busy} onclick={() => show("delete", displayCopy(folder), folder)}>{t("删除本地副本")}</button>
    {/if}
    {#if game && !game.offline}
      <button class="quiet" disabled={busy} onclick={() => void run(async () => { await library.relocate(game.id); })}>{t("重新定位…")}</button>
    {/if}
  </div>
{/snippet}

{#if device.phone}<PhoneSettingsHeader />{/if}
<main class="scroll">
  <div class="page">
    {#if !device.phone}<button class="quiet back" onclick={() => { if (!nav.requestBack()) nav.back(); }}><Icon name="chevron-left" size={15} />{t("返回")}</button>{/if}
    <h1>{t(gameId ? "游戏文件" : "游戏存储")}</h1>
    {#if problem}<p role="alert" class="destructive">{problem}</p>{/if}
    {#if busy || loading}<p role="status" class="sub">{t(busy ? "正在处理…" : "正在读取文件夹…")}</p>{/if}
    {#if game}
      <div class="game">
        <Cover {game} style="width: 65px; flex: none" />
        <div><h2>{game.title}</h2>{#if game.offline || location}<span class="sub">{t(locationLabel)}</span>{/if}</div>
      </div>
      <div class="mono path">{location?.displayPath ?? ""}</div>
      {#if game.offline}
        <button class="primary" disabled={busy} onclick={() => void run(async () => { if (preview) game.path = "~/Games/" + game.title; await library.relocate(game.id); })}>{t("重新定位…")}</button>
      {:else if location}
        {#if gameFolder}<p class="sub">{storageSize(gameFolder.bytes)} · {t("关联 {0} 个游戏", gameStorage.linked(gameFolder).length)}</p>{/if}
        {@render actions(location.displayPath, gameFolder, location.canReveal)}
      {/if}
    {:else if !gameId && !loading}
      <div class="summary"><strong>{storageSize(gameStorage.total)}</strong><span>{t("Hitode 内部副本")}</span></div>
      <div class="folders">
        {#each gameStorage.folders as folder (folder.id)}
          {@const linked = gameStorage.linked(folder)}
          {@const stack = linked.slice(0, 3)}
          {@const open = expanded === folder.id}
          <section class="folder">
            <button class="folder-row" aria-expanded={open} onclick={() => expanded = open ? null : folder.id}>
              <span class="stack" aria-hidden="true">
                {#if !stack.length}<Icon name="folder" size={24} />{/if}
                {#each stack as item, i (item.id)}
                  {@const offset = i - (stack.length - 1) / 2}
                  <span class="fan-card" style:transform={`translateX(${offset * 16}px) rotate(${offset * 26}deg)`} style:z-index={stack.length - i}><Cover game={item} /></span>
                {/each}
              </span>
              <span class="folder-name"><strong>{folder.name}</strong><span class="sub">{linked.length ? t("关联 {0} 个游戏", linked.length) : t("未关联游戏")}</span></span>
              <span class="size">{storageSize(folder.bytes)}</span>
              <Icon name={open ? "chevron-down" : "chevron-right"} size={15} />
            </button>
            {#if open}
              <div class="expanded">
                <div class="mono path">{displayCopy(folder)}</div>
                {#if linked.length}
                  <div class="sub">{t("关联游戏")}</div>
                  <div class="linked">
                    {#each linked as item (item.id)}
                      <button class="quiet linked-game" onclick={() => openGame(item.id)}>
                        <Cover game={item} style="width: 52px" /><span>{item.title}</span>
                      </button>
                    {/each}
                  </div>
                {/if}
                {@render actions(displayCopy(folder), folder, preview || device.platform !== "ios")}
              </div>
            {/if}
          </section>
        {/each}
        {#if !gameStorage.folders.length}<p class="sub">{t("没有内部副本")}</p>{/if}
      </div>
    {/if}
  </div>
</main>

<dialog bind:this={dialog} onclose={() => { dialogOpen = false; }} aria-labelledby="storage-dialog-title" oncancel={(e) => { if (busy) e.preventDefault(); }}>
  <h2 id="storage-dialog-title">{t(operation === "delete" ? "删除本地副本" : operation === "copy" ? "复制到其他位置…" : operation === "copied" ? "复制完成" : "打开所在文件夹")}</h2>
  <div class="mono path">{path}</div>
  {#if problem}<p role="alert" class="destructive">{problem}</p>{/if}
  {#if busy}<p role="status" class="sub">{t("正在处理…")}</p>{/if}
  {#if operation === "delete" && active}
    {@const folder = active}
    {@const linked = gameStorage.linked(folder)}
    <p>{t("此文件夹里的存档和补丁也会一起删除。")}</p>
    {#if linked.length}<p>{t("这些游戏会保留在库中：{0}", linked.map((item) => item.title).join("、"))}</p>{/if}
    <div class="actions"><button class="secondary" disabled={busy} onclick={() => dialog.close()}>{t("取消")}</button><button class="primary destructive-button" disabled={busy} onclick={() => deleteCopy(folder)}>{t("删除本地副本")}</button></div>
  {:else if operation === "copy"}
    <div class="actions"><button class="secondary" disabled={busy} onclick={() => dialog.close()}>{t("取消")}</button><button class="primary" onclick={() => operation = "copied"}>{t("复制到这里")}</button></div>
  {:else if operation === "copied" && active}
    {@const folder = active}
    <div class="actions">
      <button class="secondary" disabled={busy} onclick={() => dialog.close()}>{t("完成")}</button>
      {#if exportedPath && gameStorage.linked(folder).length}
        {@const destination = exportedPath}
        <button class="primary" disabled={busy} onclick={() => void run(async () => { await gameStorage.moveLinks(folder, destination); dialog.close(); })}>{t("改用新位置")}</button>
      {/if}
    </div>
  {:else}
    <div class="folder-contents mono">Game.exe<br />Data/<br />Save/</div>
    <button class="secondary" disabled={busy} onclick={() => dialog.close()}>{t("关闭")}</button>
  {/if}
</dialog>

<style>
  main { flex: 1 1 0; min-height: 0; overflow: auto; }
  .page { box-sizing: border-box; max-width: 760px; margin: 0 auto; padding: 26px 28px 40px; }
  h1 { margin: 0 0 18px; font-size: 25px; }
  h2 { margin: 0; font-size: 17px; }
  .back { display: inline-flex; align-items: center; gap: 3px; margin: 0 0 10px -7px; }
  .game, .summary, .actions { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; }
  .game { margin-bottom: 14px; }
  .sub, .summary span { color: var(--fg3); font-size: 13px; }
  .summary { align-items: baseline; margin-bottom: 18px; }
  .summary strong { font-size: 24px; font-variant-numeric: tabular-nums; }
  .folder { border-top: 1px solid var(--line); }
  .folder-row { width: 100%; border: 0; background: transparent; color: var(--fg); display: grid; grid-template-columns: 60px minmax(0,1fr) auto 15px; align-items: center; gap: 10px; padding: 8px 4px; text-align: start; cursor: pointer; }
  .folder-row:hover { background: var(--hover); }
  .folder-row:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .folder-name { display: grid; overflow-wrap: anywhere; }
  .size { white-space: nowrap; font-variant-numeric: tabular-nums; }
  .stack { display: grid; place-items: center; position: relative; height: 52px; color: var(--fg3); }
  .fan-card { width: 30px; position: absolute; }
  .expanded { padding: 6px 4px 16px; }
  .path { margin: 8px 0 12px; }
  .linked { display: flex; gap: 4px; flex-wrap: wrap; margin: 2px 0 12px; }
  .linked-game { width: 78px; display: grid; justify-items: start; gap: 5px; overflow-wrap: anywhere; text-align: start; }
  .destructive { color: var(--danger); }
  .destructive-button { background: var(--danger); }
  :global(:root[data-theme="dark"]) .destructive-button { color: var(--bg); }
  dialog { box-sizing: border-box; color: var(--fg); background: var(--pop); border: 1px solid var(--line); border-radius: 14px; padding: 18px; width: min(440px, calc(100% - 28px)); }
  dialog::backdrop { background: #0005; }
  dialog .actions { justify-content: flex-end; margin-top: 14px; }
  .folder-contents { margin: 8px 0 16px; line-height: 1.9; }
  :global(:root[data-device="phone"]) .page { padding: 8px calc(20px + var(--safe-right)) calc(24px + var(--safe-bottom)) calc(20px + var(--safe-left)); }
</style>
