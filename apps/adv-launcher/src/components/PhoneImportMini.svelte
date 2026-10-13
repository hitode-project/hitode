<script lang="ts" generics="T extends string">
  interface Props {
    options: { value: T; label: string }[];
    value: T;
    onpick: (value: T) => void;
    label?: string;
    labelledby?: string;
    mono?: boolean;
    /** Narrower buttons with smaller text, as in the install summary. */
    dense?: boolean;
  }

  let { options, value, onpick, label, labelledby, mono = false, dense = false }: Props = $props();
</script>

<!-- Small segmented control used inside list rows. -->
<div class="mini" class:dense role="group" aria-label={label} aria-labelledby={labelledby}>
  {#each options as o (o.value)}
    <button class:mono class:on={o.value === value} aria-pressed={o.value === value} onclick={() => onpick(o.value)}>{o.label}</button>
  {/each}
</div>

<style>
  .mini {
    flex: 0 0 auto;
    display: inline-flex;
    padding: 2px;
    border-radius: 8px;
    background: var(--field);
    gap: 2px;
  }
  .mini button {
    all: unset;
    box-sizing: border-box;
    height: 32px;
    padding: 0 12px;
    display: grid;
    place-items: center;
    border-radius: 6px;
    font-size: 13px;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .mini.dense button {
    padding: 0 10px;
    font-size: 12px;
  }
  .mini button.mono {
    font-family: var(--font-mono);
  }
  .mini button.on {
    background: var(--seg-on);
    color: var(--fg);
    font-weight: 500;
  }
  .mini.dense button.on {
    font-weight: 400;
  }
  .mini button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
