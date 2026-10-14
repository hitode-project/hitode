<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { setPref } from "../lib/settings-form.svelte";
  import type { RangeStats } from "../lib/stats";

  // The range's total play time, its comparison, and a note while tracking is off.
  let { data }: { data: RangeStats } = $props();
</script>

<section class="hero">
  <span class="label">{data.heroLabel}</span>
  <span class="total">{data.totalHours.toFixed(1)}<span class="unit">{t("小时")}</span></span>
  <span class="delta">{data.delta}</span>
</section>

{#if !prefs.trackPlayTime}
  <div class="notice">
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="8" cy="8" r="6"></circle><path d="M8 7.2v4M8 4.9h.01"></path></svg>
    <span style="flex: 1">{t("记录游玩时间已关闭，之后的游玩不会计入统计。")}</span>
    <button class="link" onclick={() => setPref("trackPlayTime", true)}>{t("开启")}</button>
  </div>
{/if}

<style>
  .hero {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .label {
    font-size: 13px;
    color: var(--fg3);
  }
  .total {
    font-size: 56px;
    line-height: 1.1;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .unit {
    font-size: 18px;
    font-weight: 500;
    color: var(--fg2);
    margin-left: 6px;
    letter-spacing: 0;
  }
  .delta {
    font-size: 13px;
    color: var(--fg2);
  }
  .link {
    flex: 0 0 auto;
  }
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
