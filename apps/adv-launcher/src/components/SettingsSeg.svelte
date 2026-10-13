<script lang="ts" generics="T extends string">
  interface Option {
    value: T;
    label: string;
    lang?: string;
  }

  interface Props {
    label: string;
    options: Option[];
    value: T;
    onpick: (value: T) => void;
    /** Slightly smaller buttons, as in the per-game settings. */
    compact?: boolean;
  }

  let { label, options, value, onpick, compact = false }: Props = $props();
</script>

<div class="seg" class:compact role="group" aria-label={label}>
  {#each options as o (o.value)}
    <button class:on={value === o.value} lang={o.lang} aria-pressed={value === o.value} onclick={() => onpick(o.value)}>
      {o.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    flex: 0 0 auto;
    display: inline-flex;
    padding: 2px;
    border-radius: 9px;
    background: var(--field);
    gap: 2px;
  }
  button {
    all: unset;
    box-sizing: border-box;
    height: 30px;
    padding: 0 12px;
    border-radius: 7px;
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .compact button {
    height: 28px;
    padding: 0 11px;
  }
  button:hover {
    color: var(--fg);
  }
  button.on {
    background: var(--seg-on);
    color: var(--fg);
    font-weight: 500;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .seg {
    --seg-on: #ffffff;
  }
  :global(:root[data-theme="dark"]) .seg {
    --seg-on: #2c2c2f;
  }
  .compact {
    --seg-on: #ffffff;
  }
  :global(:root[data-theme="dark"]) .compact {
    --seg-on: #3a3a3e;
  }
</style>
