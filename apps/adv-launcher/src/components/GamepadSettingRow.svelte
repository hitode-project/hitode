<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { gamepad } from "../lib/gamepad.svelte";
  import { nav } from "../lib/nav.svelte";

  const statusText = $derived.by(() => {
    const count = gamepad.connectedIds.length;
    if (count > 1) return t("已连接 {0} 个", count);
    if (count === 1) {
      const dev = gamepad.config.devices.find((d) => d.id === gamepad.connectedIds[0]);
      return dev?.name ?? t("已连接");
    }
    return t("未连接");
  });
</script>

{#if gamepad.showInSettings}
  <div class="row">
    <span class="t gp-title">
      <span class="dot" class:live={gamepad.hasConnected} aria-hidden="true"></span>
      <span>{t("外接手柄")}</span>
      <span class="status">{statusText}</span>
    </span>
    <button class="quiet" onclick={() => nav.go({ name: "gamepad" })}>{t("管理")}</button>
  </div>
{/if}

<style>
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 38px;
  }
  .t {
    font-size: 14px;
    color: var(--fg);
    min-width: 0;
  }
  .gp-title {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--fg3);
    flex: none;
  }
  .dot.live {
    background: #34c759;
  }
  .status {
    font-size: 12px;
    color: var(--fg3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .quiet {
    flex: none;
  }
  :global(:root[data-device="phone"]) .row {
    min-height: 48px;
  }
  :global(:root[data-device="phone"]) .t {
    font-size: 15px;
  }
  :global(:root[data-device="phone"]) .quiet {
    padding: 8px 12px;
    font-size: 14px;
  }
</style>
