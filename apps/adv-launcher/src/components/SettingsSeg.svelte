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
    /**
     * Size and selected-button tone: "page" for the settings screen, "sheet" for
     * the per-game settings, and their phone counterparts with larger targets.
     */
    variant?: "page" | "sheet" | "phone" | "phone-sheet";
  }

  let { label, options, value, onpick, variant = "page" }: Props = $props();
</script>

<div class="seg {variant}" role="group" aria-label={label}>
  {#each options as o (o.value)}
    <button class:on={value === o.value} lang={o.lang} aria-pressed={value === o.value} onclick={() => onpick(o.value)}>
      {o.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    --seg-on: #ffffff;
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
    text-align: center;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .sheet button {
    height: 28px;
    padding: 0 11px;
  }
  .phone button {
    height: 32px;
    padding: 0 11px;
  }
  .phone-sheet {
    border-radius: 10px;
  }
  .phone-sheet button {
    min-width: 44px;
    height: 36px;
    padding: 0 11px;
    border-radius: 8px;
    font-size: 14px;
  }
  @media (hover: hover) {
    button:hover {
      color: var(--fg);
    }
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
  :global(:root[data-theme="dark"]) .seg {
    --seg-on: #2c2c2f;
  }
  :global(:root[data-theme="dark"]) .sheet,
  :global(:root[data-theme="dark"]) .phone-sheet {
    --seg-on: #3a3a3e;
  }
</style>
