<script lang="ts">
  import { prefs } from "../lib/prefs.svelte";
  import { ACCENTS, setPref } from "../lib/settings-form.svelte";

  /** `touch`: the phone's larger swatches with an inner ring. */
  let { touch = false }: { touch?: boolean } = $props();
</script>

<div class="swatches" class:touch role="group" aria-label="强调色">
  {#each ACCENTS as a (a.value)}
    <button
      class="swatch"
      class:on={prefs.accent === a.value}
      aria-label={a.label}
      aria-pressed={prefs.accent === a.value}
      onclick={() => setPref("accent", a.value)}
    >
      <span style:background={a.color}></span>
    </button>
  {/each}
</div>

<style>
  .swatches {
    display: flex;
    gap: 10px;
  }
  .swatches.touch {
    gap: 4px;
  }
  .swatch {
    all: unset;
    box-sizing: border-box;
    width: 28px;
    height: 28px;
    border-radius: 14px;
    cursor: pointer;
    display: grid;
    place-items: center;
  }
  .swatch span {
    width: 20px;
    height: 20px;
    border-radius: 10px;
  }
  .swatch.on {
    box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--fg3);
  }
  .touch .swatch {
    width: 36px;
    height: 36px;
    border-radius: 18px;
  }
  .touch .swatch span {
    width: 22px;
    height: 22px;
    border-radius: 11px;
  }
  .touch .swatch.on {
    box-shadow: inset 0 0 0 2px var(--fg3);
  }
  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
