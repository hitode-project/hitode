<script lang="ts" generics="T extends string">
  interface Props {
    options: { value: T; label: string }[];
    value: T;
    onpick: (value: T) => void;
    label?: string;
    labelledby?: string;
    mono?: boolean;
  }

  let { options, value, onpick, label, labelledby, mono = false }: Props = $props();
</script>

<!-- Small segmented control used inside list rows. -->
<div class="mini" role="group" aria-label={label} aria-labelledby={labelledby}>
  {#each options as o (o.value)}
    <button class:mono class:on={o.value === value} aria-pressed={o.value === value} onclick={() => onpick(o.value)}>{o.label}</button>
  {/each}
</div>

<style>
  .mini {
    --seg-on: #ffffff;
    display: inline-flex;
    padding: 2px;
    border-radius: 7px;
    background: var(--field);
    gap: 2px;
  }
  :global(:root[data-theme="dark"]) .mini {
    --seg-on: #2c2c2f;
  }
  .mini button {
    all: unset;
    box-sizing: border-box;
    height: 26px;
    padding: 0 9px;
    display: grid;
    place-items: center;
    border-radius: 5px;
    font-size: 12px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .mini button.mono {
    font-family: var(--font-mono);
  }
  .mini button.on {
    background: var(--seg-on);
    color: var(--fg);
    font-weight: 500;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
  }
  .mini button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
