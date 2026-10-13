<script lang="ts">
  import type { Snippet } from "svelte";
  import { library } from "../lib/library.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import type { Game } from "../lib/types";

  interface Props {
    game: Game;
    /** Extra content over the art (badges, reveal buttons). */
    children?: Snippet<[{ hidden: boolean }]>;
    style?: string;
  }

  let { game, children, style = "" }: Props = $props();

  /** Adult covers stay blurred until revealed in the detail panel. */
  const hidden = $derived(game.nsfw && prefs.blurAdult && !library.revealed[game.id]);
</script>

<div class="cv" class:gone={game.offline !== null} {style}>
  <div class="art {game.cover}" class:blur={hidden}></div>
  {@render children?.({ hidden })}
</div>

<style>
  .cv {
    position: relative;
    aspect-ratio: 5 / 7;
    border-radius: 6px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--line), 0 1px 2px rgba(0, 0, 0, 0.06);
    transition: transform 0.18s ease, box-shadow 0.18s ease;
  }
  .art {
    position: absolute;
    inset: 0;
  }
  .art::after {
    content: "";
    position: absolute;
    inset: 0;
    background: radial-gradient(38% 52% at 58% 100%, rgba(255, 255, 255, 0.26), rgba(255, 255, 255, 0) 70%),
      linear-gradient(180deg, rgba(0, 0, 0, 0) 62%, rgba(0, 0, 0, 0.14));
  }
  .art.blur {
    filter: blur(16px) saturate(0.8);
    transform: scale(1.25);
  }
  .gone .art {
    filter: grayscale(1);
    opacity: 0.42;
  }
  .gone .art.blur {
    filter: blur(16px) grayscale(1);
  }
</style>
