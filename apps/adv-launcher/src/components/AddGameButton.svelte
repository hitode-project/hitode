<script lang="ts">
  import Icon from "./Icon.svelte";
  import { t } from "../lib/i18n.svelte";
  import { nav } from "../lib/nav.svelte";

  let { compact = false }: { compact?: boolean } = $props();
  let open = $state(false);
  let wrapper: HTMLDivElement;
  let trigger: HTMLButtonElement;
  let first = $state<HTMLButtonElement>();
  $effect(() => { if (open) first?.focus(); });

  function close() {
    open = false;
    trigger.focus();
  }

  $effect(() => {
    if (open) return nav.onBack(() => { close(); return true; });
  });

  function pick(name: "import" | "scan") {
    open = false;
    nav.go({ name });
  }
</script>

<svelte:window onpointerdown={(e) => { if (open && e.target instanceof Node && !wrapper.contains(e.target)) open = false; }} onkeydown={(e) => { if (open && e.key === "Escape") { e.preventDefault(); close(); } }} />

<div class="add-game" bind:this={wrapper}>
  <button class={compact ? "icon-btn" : "primary"} aria-label={t("添加游戏")} aria-expanded={open} bind:this={trigger} onclick={() => open = !open}>
    <Icon name="plus" size={compact ? 20 : 16} stroke={1.5} />
    {#if !compact}<span>{t("添加游戏")}</span>{/if}
  </button>
  {#if open}
    <div class="options" role="group" aria-label={t("添加游戏")}>
      <button class="quiet" bind:this={first} onclick={() => pick("import")}>{t("导入单个游戏")}</button>
      <button class="quiet" onclick={() => pick("scan")}>{t("批量导入")}</button>
    </div>
  {/if}
</div>

<style>
  .add-game { position: relative; }
  .options { position: absolute; top: calc(100% + 6px); right: 0; z-index: 8; min-width: 170px; display: grid; padding: 5px; text-align: left; background: var(--pop); border: 1px solid var(--line); border-radius: 12px; box-shadow: 0 8px 28px #0002; }
  .options button { padding: 9px 10px; }
  :global(:root[data-device="phone"]) .options button { min-height: 26px; }
</style>
