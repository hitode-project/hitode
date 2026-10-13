<script lang="ts">
  import SettingsGameForm from "../components/SettingsGameForm.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";

  let { gameId }: { gameId: string } = $props();

  const game = $derived(library.get(gameId));
</script>

{#if game}
  {#key game.id}
    <SettingsGameForm {game} />
  {/key}
{:else}
  <main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto; display: grid; place-items: center">
    <div class="missing">
      <h1>找不到这个游戏</h1>
      <p>它可能已从游戏库中移除。</p>
      <button class="secondary" onclick={() => nav.home()}>返回游戏库</button>
    </div>
  </main>
{/if}

<style>
  .missing {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 32px;
    text-align: center;
  }
  h1 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
  }
  p {
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--fg3);
  }
</style>
