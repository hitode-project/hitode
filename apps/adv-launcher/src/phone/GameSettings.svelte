<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import PhoneSettingsGameForm from "../components/PhoneSettingsGameForm.svelte";
  import PhoneSettingsHeader from "../components/PhoneSettingsHeader.svelte";
  import { library } from "../lib/library.svelte";
  import { nav } from "../lib/nav.svelte";

  let { gameId }: { gameId: string } = $props();

  const game = $derived(library.get(gameId));
</script>

{#if game}
  {#key game.id}
    <PhoneSettingsGameForm {game} />
  {/key}
{:else}
  <div class="screen">
    <PhoneSettingsHeader />
    <div class="missing">
      <h1>{t("找不到这个游戏")}</h1>
      <p>{t("它可能已从游戏库中移除。")}</p>
      <button class="secondary" onclick={() => nav.home()}>{t("返回游戏库")}</button>
    </div>
  </div>
{/if}

<style>
  .screen {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .missing {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 32px 32px calc(var(--safe-bottom) + 32px);
    text-align: center;
  }
  h1 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
  }
  p {
    margin: 0 0 12px;
    font-size: 15px;
    color: var(--fg3);
  }
  .secondary {
    height: 44px;
    border-radius: 22px;
  }
</style>
