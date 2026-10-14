<script lang="ts">
  import Vndb from "./Vndb.svelte";
  import { t } from "../lib/i18n.svelte";
  import type { Game } from "../lib/types";
  let { game }: { game: Game } = $props();
  let open = $state(false);
</script>
<button class="quiet" onclick={() => open = true}>VNDB · {game.vndb?.id ?? t(game.vndbChecked ? "待手动选择" : "未关联")}</button>
{#if open}<Vndb {game} onclose={() => open = false} />{/if}
