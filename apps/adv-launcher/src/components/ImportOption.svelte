<script lang="ts">
  import type { Snippet } from "svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    /** Radio group name. */
    name: string;
    checked: boolean;
    onpick: () => void;
    /** Show the trailing check mark when selected. */
    check?: boolean;
    /** Shown but not choosable. */
    disabled?: boolean;
    style?: string;
    children: Snippet;
  }

  let { name, checked, onpick, check = true, disabled = false, style = "", children }: Props = $props();
</script>

<!-- One choice of a radio group, drawn as a selectable row. -->
<label class="opt" class:on={checked} class:disabled {style}>
  <input type="radio" {name} {checked} {disabled} onchange={onpick} />
  {@render children()}
  {#if check}<span class="chk"><Icon name="check" stroke={1.8} /></span>{/if}
</label>

<style>
  .opt.disabled {
    opacity: 0.5;
    cursor: default;
  }
  .opt.disabled:hover {
    background: none;
  }
  .opt {
    position: relative;
    display: flex;
    align-items: center;
    gap: 14px;
    min-height: 52px;
    padding: 0 16px;
    border-radius: 12px;
    cursor: pointer;
    box-sizing: border-box;
    transition: background 0.12s ease;
  }
  .opt:hover {
    background: var(--hover);
  }
  .opt.on {
    background: var(--sel);
  }
  .opt input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .opt:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .chk {
    flex: 0 0 auto;
    margin-left: auto;
    display: flex;
    color: var(--accent);
    opacity: 0;
  }
  .opt.on .chk {
    opacity: 1;
  }
  @media (prefers-reduced-motion: reduce) {
    .opt {
      transition: none;
    }
  }
</style>
