<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import type { Snippet } from "svelte";

  // A full-screen result of the phone report screen (已提交, 没有连上服务器, 已保存):
  // a centered badge and title with the actions stacked at the bottom.
  interface Props {
    id: string;
    title: string;
    ok?: boolean;
    badge: Snippet;
    /** Lines under the title. */
    children?: Snippet;
    actions: Snippet;
    /** Stack the actions 4px apart instead of 8px. */
    tight?: boolean;
    onclose?: () => void;
  }

  let { id, title, ok = false, badge, children, actions, tight = false, onclose }: Props = $props();
</script>

<div class="res fade" class:closable={!!onclose} role="dialog" aria-modal="true" aria-labelledby={id}>
  {#if onclose}
    <button class="x" aria-label={t("关闭")} onclick={onclose}>
      <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"></path></svg>
    </button>
  {/if}
  <div class="mid">
    <div class="badge" class:ok aria-hidden="true">{@render badge()}</div>
    <h1 {id}>{title}</h1>
    {@render children?.()}
  </div>
  <div class="acts" class:tight>{@render actions()}</div>
</div>

<style>
  .res {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 0 max(20px, var(--safe-right)) max(var(--safe-bottom), 16px) max(20px, var(--safe-left));
  }
  .res.closable {
    padding-top: calc(var(--safe-top) + 6px);
  }
  .x {
    all: unset;
    box-sizing: border-box;
    width: 44px;
    height: 44px;
    margin-left: -12px;
    display: grid;
    place-items: center;
    border-radius: 22px;
    color: var(--fg);
    cursor: pointer;
  }
  .x:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .mid {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
  }
  .badge {
    width: 56px;
    height: 56px;
    border-radius: 28px;
    display: grid;
    place-items: center;
    background: var(--field);
    color: var(--fg);
  }
  .badge.ok {
    background: var(--tint);
    color: var(--accent);
  }
  h1 {
    margin: 20px 0 0;
    font-size: 24px;
    font-weight: 600;
  }
  .acts {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .acts.tight {
    gap: 4px;
  }
  .fade {
    animation: fade 0.22s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fade {
      animation: none;
    }
  }
</style>
