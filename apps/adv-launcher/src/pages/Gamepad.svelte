<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { device } from "../lib/device.svelte";
  import { nav } from "../lib/nav.svelte";
  import PhoneSettingsHeader from "../components/PhoneSettingsHeader.svelte";
  import GamepadPanel from "../components/GamepadPanel.svelte";
  import Icon from "../components/Icon.svelte";

  let picking = $state(false);
</script>

<div class="screen" class:phone={device.phone}>
  {#if device.phone && !picking}
    <PhoneSettingsHeader label={t("返回设置")} />
  {/if}
  <main class="scroll" class:picker-active={device.phone && picking}>
    <div class="page" class:picker-page={device.phone && picking}>
      {#if !device.phone}
        <button
          class="quiet back"
          onclick={() => {
            if (!nav.requestBack()) nav.back();
          }}
        >
          <Icon name="chevron-left" size={15} />{t("返回")}
        </button>
      {/if}
      {#if !(device.phone && picking)}
        <h1>{t("外接手柄")}</h1>
      {/if}
      <GamepadPanel onpickingchange={(v) => (picking = v)} />
    </div>
  </main>
</div>

<style>
  .screen {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  main {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
  }
  main.picker-active {
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .page {
    max-width: 780px;
    margin: 0 auto;
    padding: 28px 36px 56px;
  }
  .phone .page {
    max-width: 640px;
    padding: 8px calc(var(--safe-right) + 18px) calc(var(--safe-bottom) + 32px)
      calc(var(--safe-left) + 18px);
  }
  .phone .page.picker-page {
    flex: 1 1 0;
    min-height: 0;
    width: 100%;
    padding: var(--safe-top) var(--safe-right) var(--safe-bottom) var(--safe-left);
    display: flex;
    flex-direction: column;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin: 0 0 12px -8px;
  }
  h1 {
    margin: 0 0 18px;
    font-size: 24px;
    font-weight: 600;
    letter-spacing: -0.02em;
  }
  .phone h1 {
    font-size: 26px;
    margin: 4px 0 16px;
  }
</style>
