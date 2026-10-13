<script lang="ts">
  // Blurred cover art behind the fullscreen launch and report screens.
  interface Props {
    cover: string;
    /** Extra dimmed, blurred layer under a dialog. */
    dim?: boolean;
  }

  let { cover, dim = false }: Props = $props();
</script>

<div class="backdrop" aria-hidden="true">
  <div class="bd {cover}"></div>
  <div class="scrim"></div>
  {#if dim}
    <div class="dim"></div>
  {/if}
</div>

<style>
  .backdrop {
    position: absolute;
    inset: 0;
    overflow: clip;
  }
  .bd {
    position: absolute;
    inset: -20%;
    filter: blur(90px) saturate(1.1);
    opacity: 0.55;
  }
  .scrim {
    position: absolute;
    inset: 0;
    background: radial-gradient(55% 65% at 50% 42%, rgba(244, 244, 242, 0.5) 0%, rgba(244, 244, 242, 0.94) 100%);
  }
  :global(:root[data-theme="dark"]) .scrim {
    background: radial-gradient(55% 65% at 50% 42%, rgba(20, 20, 21, 0.45) 0%, rgba(20, 20, 21, 0.95) 100%);
  }
  .dim {
    position: absolute;
    inset: 0;
    background: rgba(24, 24, 22, 0.16);
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
  }
  :global(:root[data-theme="dark"]) .dim {
    background: rgba(0, 0, 0, 0.45);
  }
</style>
