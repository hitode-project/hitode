<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    /** The game's name, for the label. */
    name: string;
    exes: string[];
    value: string;
    onpick: (path: string) => void;
    /** Phone rows use taller controls. */
    phone?: boolean;
  }

  let { name, exes, value, onpick, phone = false }: Props = $props();
</script>

<!-- A scan row's main program: the suggested one, or a native menu of the others. -->
{#if exes.length > 1}
  <span class="exe" class:phone>
    <select class="mono" aria-label={t("{0} 的主程序", name)} onchange={(e) => onpick(e.currentTarget.value)}>
      {#each exes as exe (exe)}
        <option value={exe} selected={exe === value}>{exe}</option>
      {/each}
    </select>
    <Icon name="chevron-down" size={11} stroke={1.7} />
  </span>
{:else}
  <span class="exe text mono" class:phone>{value}</span>
{/if}

<style>
  .exe {
    position: relative;
    display: inline-flex;
    align-items: center;
    min-width: 0;
    max-width: 100%;
  }
  .text {
    display: block;
    color: var(--fg2);
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  select {
    appearance: none;
    -webkit-appearance: none;
    box-sizing: border-box;
    min-width: 0;
    max-width: 100%;
    height: 26px;
    padding: 0 24px 0 9px;
    border: 0;
    border-radius: 7px;
    background: var(--field);
    color: var(--fg);
    font-size: 12px;
    text-overflow: ellipsis;
    cursor: pointer;
  }
  .phone select {
    height: 32px;
    padding: 0 26px 0 11px;
    border-radius: 8px;
  }
  select:hover {
    background: var(--sel);
  }
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .exe :global(svg) {
    position: absolute;
    right: 8px;
    pointer-events: none;
    color: var(--fg3);
  }
</style>
