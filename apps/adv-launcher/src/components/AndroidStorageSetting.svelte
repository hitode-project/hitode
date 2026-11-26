<script lang="ts">
  import { onMount } from "svelte";
  import { androidStorage } from "../lib/android-access.svelte";
  import { device } from "../lib/device.svelte";
  import { t } from "../lib/i18n.svelte";

  onMount(() => {
    if (device.platform === "android") void androidStorage.refresh();
  });
</script>

{#if device.platform === "android" && androidStorage.supported}
  <div class="row">
    <span class="t">{t("访问外部文件夹")}</span>
    <span class="state">{t(androidStorage.granted ? "已开启" : "未开启")}</span>
    <button
      class="quiet"
      disabled={androidStorage.busy}
      onclick={() => void androidStorage.request(androidStorage.granted)}
    >
      {t(androidStorage.granted ? "设置" : "去授权")}
    </button>
  </div>
{/if}

<style>
  .row {
    min-height: 56px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    border-bottom: 1px solid var(--line);
  }
  .state {
    margin-left: auto;
    color: var(--fg3);
    font-size: 13px;
    white-space: nowrap;
  }
  .t {
    flex: 1;
    min-width: 0;
    font-size: 14px;
    color: var(--fg);
  }
  .quiet {
    box-sizing: border-box;
    height: 32px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    border-radius: 16px;
    white-space: nowrap;
  }
  :global(:root[data-device="phone"]) .t {
    font-size: 15px;
  }
  :global(:root[data-device="phone"]) .quiet {
    height: 36px;
    border-radius: 18px;
    font-size: 14px;
  }
</style>
