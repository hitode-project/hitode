<script lang="ts" module>
  export type MenuCommand = { window: number; root: number; menu: number; position: number; id: number };
  export type MenuEntry = {
    text: string;
    separator: boolean;
    enabled: boolean;
    checked: boolean;
    command: MenuCommand;
    submenu: number;
    position: number;
    children: MenuEntry[];
  };
  /** The game's menu bar, or a popup it waits on (mobile_view.rs `game_menu`). */
  export type GuestMenu = { window: number; root: number; popup: [number, number] | null; items: MenuEntry[] };
</script>

<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../lib/i18n.svelte";
  import { call } from "../lib/bridge";
  import { modal } from "../lib/overlay";
  import { nav } from "../lib/nav.svelte";
  import Icon from "./Icon.svelte";

  /**
   * The game's menu as a sheet on mobile: the bar opens from the pause menu,
   * and a popup the game tracks opens on its own until it is answered.
   */
  let { menu, onclose }: { menu: GuestMenu; onclose: () => void } = $props();

  type Input =
    | { kind: "open"; window: number; root: number; menu: number; position: number | null }
    | { kind: "choose"; command: MenuCommand; popup: boolean }
    | { kind: "cancel"; window: number; root: number };
  function send(input: Input) {
    void call("game_menu", { input }, null).catch((error) => console.error("game_menu failed", error));
  }

  /** Positions of the open submenus, so updated items replace stale ones. */
  let path = $state<number[]>([]);
  const level = $derived.by(() => {
    let items = menu.items;
    let title = "";
    for (const position of path) {
      const entry = items[position];
      if (!entry) break;
      title = entry.text;
      items = entry.children;
    }
    return { items, title };
  });

  // The bar initializes once as it opens (WM_INITMENU); a popup did so itself.
  onMount(() => {
    const { window, root, popup } = menu;
    if (!popup) send({ kind: "open", window, root, menu: 0, position: null });
  });

  function open(entry: MenuEntry) {
    if (!entry.enabled) return;
    if (entry.submenu) {
      send({ kind: "open", window: menu.window, root: menu.root, menu: entry.submenu, position: entry.position });
      path = [...path, entry.position];
    } else {
      send({ kind: "choose", command: entry.command, popup: menu.popup !== null });
      onclose();
    }
  }

  function close() {
    if (menu.popup) send({ kind: "cancel", window: menu.window, root: menu.root });
    onclose();
  }

  function back() {
    if (path.length) path = path.slice(0, -1);
    else close();
  }

  $effect(() =>
    nav.onBack(() => {
      back();
      return true;
    }),
  );
</script>

<div class="layer fade" {@attach modal}>
  <button class="scrim" aria-label={t("关闭")} onclick={close}></button>
  <div class="sheet" role="menu" aria-label={t("游戏菜单")} lang="ja">
    <div class="head">
      {#if path.length}
        <button class="nav" aria-label={t("返回")} onclick={back}><Icon name="chevron-left" size={18} /></button>
      {/if}
      <span>{level.title || t("游戏菜单")}</span>
    </div>
    <div class="items">
      {#each level.items as entry, i (i)}
        {#if entry.separator}
          <div class="sep"></div>
        {:else}
          <button class="item" role="menuitem" disabled={!entry.enabled} onclick={() => open(entry)}>
            <span class="check">{#if entry.checked}<Icon name="check" size={16} />{/if}</span>
            <span class="text">{entry.text}</span>
            {#if entry.submenu}<Icon name="chevron-right" size={16} />{/if}
          </button>
        {/if}
      {/each}
    </div>
  </div>
</div>

<style>
  .layer {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    z-index: 15;
  }
  .scrim {
    all: unset;
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
  }
  .sheet {
    position: relative;
    width: min(440px, 100%);
    max-height: 80%;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    padding: 8px 8px calc(8px + env(safe-area-inset-bottom));
    border-radius: 20px 20px 0 0;
    background: var(--panel);
    border: 1px solid var(--line);
    color: var(--fg);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 4px;
    min-height: 44px;
    padding: 0 12px;
    font-size: 15px;
    font-weight: 600;
  }
  .nav {
    all: unset;
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    margin-left: -8px;
    border-radius: 18px;
    cursor: pointer;
  }
  .items {
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }
  .item {
    all: unset;
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 48px;
    padding: 0 12px;
    border-radius: 12px;
    font-size: 15px;
    cursor: pointer;
  }
  .item:active {
    background: var(--hover);
  }
  .item:disabled {
    opacity: 0.4;
  }
  .check {
    width: 16px;
    display: grid;
    place-items: center;
    color: var(--accent);
  }
  .text {
    flex: 1;
  }
  .sep {
    height: 1px;
    margin: 4px 12px;
    background: var(--line);
  }
</style>
