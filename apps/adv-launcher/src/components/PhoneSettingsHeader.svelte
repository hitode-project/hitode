<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { nav } from "../lib/nav.svelte";

  // The phone settings and statistics screens' top bar: a single back button.
  let { label = t("返回") }: { label?: string } = $props();
</script>

<header>
  <button class="back" aria-label={label} onclick={() => nav.back()}>
    <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9.5 3.5L5 8l4.5 4.5"></path></svg>
  </button>
</header>

<style>
  header {
    flex: 0 0 auto;
    padding: calc(var(--safe-top) + 6px) calc(var(--safe-right) + 8px) 0 calc(var(--safe-left) + 8px);
  }
  .back {
    all: unset;
    box-sizing: border-box;
    width: 44px;
    height: 44px;
    display: grid;
    place-items: center;
    border-radius: 22px;
    color: var(--fg);
    cursor: pointer;
  }
  .back:active {
    background: var(--field);
  }
  .back:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
