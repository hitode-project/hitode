<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { nav } from "../lib/nav.svelte";

  let { title }: { title: string } = $props();
</script>

<main style="flex: 1; display: grid; place-items: center; color: var(--fg3); font-size: 13px">
  <div style="display: flex; flex-direction: column; align-items: center; gap: 12px">
    <div>{title}{t("：尚未实现")}</div>
    <button class="quiet" onclick={() => nav.back()}>{t("返回")}</button>
  </div>
</main>
