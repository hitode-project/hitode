<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    children: Snippet;
  }

  let { children }: Props = $props();
</script>

<!-- Root of the phone adding screens: fills the phone frame and holds their tokens. -->
<div class="page">
  {@render children()}
</div>

<style>
  .page {
    --check: var(--accent);
    --seg-on: #ffffff;
    --bar: rgba(244, 244, 242, 0.94);
    position: relative;
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  :global(:root[data-theme="dark"]) .page {
    --seg-on: #2c2c2f;
    --bar: rgba(20, 20, 21, 0.94);
  }
  :global(:root[data-theme="dark"][data-accent="blue"]) .page {
    --check: #5b86e8;
  }
</style>
