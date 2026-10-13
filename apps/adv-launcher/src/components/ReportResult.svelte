<script lang="ts">
  import type { Snippet } from "svelte";

  // The small centered result dialog of the report screen (已提交, 没有连上服务器, 已保存).
  interface Props {
    id: string;
    title: string;
    ok?: boolean;
    badge: Snippet;
    children: Snippet;
    onclose?: () => void;
  }

  let { id, title, ok = false, badge, children, onclose }: Props = $props();
</script>

<div class="dlg fade" role="dialog" aria-modal="true" aria-labelledby={id}>
  {#if onclose}
    <button class="x" aria-label="关闭" onclick={onclose}>
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"></path></svg>
    </button>
  {/if}
  <div class="badge" class:ok aria-hidden="true">{@render badge()}</div>
  <h2 {id}>{title}</h2>
  {@render children()}
</div>

<style>
  .dlg {
    position: relative;
    width: 420px;
    max-width: 100%;
    max-height: 100%;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: 44px 32px 32px;
    background: var(--surface);
    border-radius: 16px;
    box-shadow: 0 30px 80px rgba(0, 0, 0, 0.22), 0 0 0 1px var(--line);
    overflow: hidden;
  }
  .x {
    all: unset;
    box-sizing: border-box;
    position: absolute;
    top: 14px;
    right: 14px;
    width: 36px;
    height: 36px;
    display: grid;
    place-items: center;
    border-radius: 18px;
    color: var(--fg2);
    cursor: pointer;
  }
  .x:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .x:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .badge {
    width: 48px;
    height: 48px;
    border-radius: 24px;
    display: grid;
    place-items: center;
    background: var(--field);
    color: var(--fg);
  }
  .badge.ok {
    background: var(--tint);
    color: var(--accent);
  }
  h2 {
    margin: 18px 0 0;
    font-size: 20px;
    font-weight: 600;
  }
  .fade {
    animation: fade 0.22s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fade {
      animation: none;
    }
  }
</style>
