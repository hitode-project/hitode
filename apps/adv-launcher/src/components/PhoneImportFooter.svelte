<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    children: Snippet;
  }

  let { children }: Props = $props();
</script>

<!-- Translucent bar over the bottom of the scrolling content. -->
<footer>
  {@render children()}
</footer>

<style>
  footer {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    padding: 14px calc(var(--safe-right) + 20px) max(14px, var(--safe-bottom)) calc(var(--safe-left) + 20px);
    background: var(--bar);
    backdrop-filter: blur(12px);
    -webkit-backdrop-filter: blur(12px);
    display: flex;
  }
  footer :global(.primary) {
    flex: 1;
    height: 48px;
    padding: 0 20px;
    border-radius: 24px;
    font-size: 15px;
  }
  /* The board dims the disabled button rather than greying it. */
  footer :global(.primary:disabled) {
    background: var(--accent);
    color: var(--on-accent);
    opacity: 0.45;
  }
</style>
