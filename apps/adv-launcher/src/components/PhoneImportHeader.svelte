<script lang="ts">
  import Icon from "./Icon.svelte";

  interface Props {
    label: string;
    onclose: () => void;
  }

  let { label, onclose }: Props = $props();
</script>

<header>
  <button class="icon-btn" aria-label={label} onclick={onclose}><Icon name="close" size={20} stroke={1.5} /></button>
</header>

<style>
  header {
    flex: 0 0 auto;
    padding: calc(var(--safe-top) + 6px) calc(var(--safe-right) + 8px) 0 calc(var(--safe-left) + 8px);
  }
  .icon-btn {
    width: 44px;
    height: 44px;
    border-radius: 22px;
    color: var(--fg);
  }
  .icon-btn:hover {
    background: none;
  }
  .icon-btn:focus-visible {
    outline-offset: 2px;
  }
</style>
