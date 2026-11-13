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
  .row { min-height: 38px; display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .state { margin-left: auto; color: var(--fg3); font-size: 13px; }
  .t { flex: 1; font-size: 14px; color: var(--fg); }
  :global(:root[data-device="phone"]) .row { min-height: 48px; }
</style>
