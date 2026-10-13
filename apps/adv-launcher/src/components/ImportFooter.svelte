<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    /** Max width of the column the footer lines up with. */
    width: number;
    children: Snippet;
  }

  let { width, children }: Props = $props();
</script>

<footer style="flex: 0 0 auto">
  <div class="bar" style="max-width: {width}px">
    {@render children()}
  </div>
</footer>

<style>
  .bar {
    margin: 0 auto;
    padding: 16px 32px 28px;
    box-sizing: border-box;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  /* The adding flows use the larger footer buttons of their boards. */
  .bar :global(.primary) {
    height: 40px;
    padding: 0 22px;
    border-radius: 20px;
    white-space: nowrap;
  }
  .bar :global(.quiet) {
    box-sizing: border-box;
    height: 40px;
    padding: 0 16px;
    display: flex;
    align-items: center;
    border-radius: 20px;
    font-size: 14px;
    white-space: nowrap;
  }
</style>
