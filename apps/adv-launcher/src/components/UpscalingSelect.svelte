<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import type { Filter } from "../lib/prefs.svelte";
  let { value, inherit, onpick }: {
    value: Filter | "inherit";
    inherit?: Filter;
    onpick: (value: Filter | "inherit") => void;
  } = $props();
  const options: { value: Filter; label: string }[] = [
    { value: "off", label: t("关") },
    { value: "anime4k", label: "Anime4K" },
    { value: "cunny", label: "CuNNy" },
    { value: "artcnn", label: "ArtCNN" },
    { value: "k7", label: "k7_modernAnime" },
  ];
</script>

<select aria-label={t("超分辨率")} {value} onchange={(event) => onpick(event.currentTarget.value as Filter | "inherit")}>
  {#if inherit !== undefined}<option value="inherit">{t("跟随全局（{0}）", options.find((o) => o.value === inherit)?.label)}</option>{/if}
  {#each options as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
</select>

<style>
  select {
    max-width: 100%;
    min-width: 160px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--field);
    color: var(--fg);
    font: inherit;
    font-size: 13px;
  }
  select:focus-visible { outline: 2px solid var(--mark); outline-offset: 2px; }
</style>
