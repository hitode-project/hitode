<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import PhoneImportOption from "./PhoneImportOption.svelte";
  import type { Enc, EncChoice } from "../lib/fake-import";

  interface Props {
    choices: EncChoice[];
    value: Enc;
    onpick: (value: Enc) => void;
    /** Smaller cards, as on the install-complete screen. */
    compact?: boolean;
  }

  let { choices, value, onpick, compact = false }: Props = $props();
</script>

<fieldset>
  <legend class="lbl">{t("编码")}</legend>
  <div class="grid">
    {#each choices as e (e.k)}
      <PhoneImportOption
        name="enc"
        checked={value === e.k}
        onpick={() => onpick(e.k)}
        check={false}
        style="flex-direction: column; align-items: flex-start; justify-content: center; gap: 4px; min-height: {compact ? 76 : 84}px; padding: 12px 14px; background: {value === e.k ? 'var(--sel)' : 'var(--field)'}"
      >
        <span class="pv" class:compact lang={e.lang}>{e.preview}</span>
        <span class="cap">{e.label}</span>
      </PhoneImportOption>
    {/each}
  </div>
</fieldset>

<style>
  fieldset {
    all: unset;
    display: block;
  }
  .lbl {
    margin: 0 0 10px;
    padding: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }
  .pv {
    font-size: 16px;
    line-height: 1.35;
    font-weight: 500;
    max-width: 100%;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pv.compact {
    font-size: 15px;
  }
  .cap {
    font-size: 12px;
    color: var(--fg3);
  }
</style>
