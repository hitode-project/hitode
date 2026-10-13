<script lang="ts">
  import Icon from "./Icon.svelte";
  import { nav } from "../lib/nav.svelte";
</script>

<main class="empty">
  <div aria-hidden="true" class="stack">
    <div class="ghost-card" style="transform: rotate(-6deg) translateY(6px); opacity: .6"></div>
    <div class="ghost-card" style="width: 76px"></div>
    <div class="ghost-card" style="transform: rotate(6deg) translateY(6px); opacity: .6"></div>
  </div>
  <h2>添加游戏文件夹</h2>
  <div class="actions">
    <button class="primary" onclick={() => nav.go({ name: "scan" })}>
      <Icon name="folder-plus" size={15} stroke={1.5} />
      <span>添加文件夹…</span>
    </button>
    <button class="quiet">支持哪些游戏？</button>
  </div>
</main>

<style>
  .empty {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    padding: 0 24px 96px;
  }
  .stack {
    display: flex;
    align-items: flex-end;
    gap: 10px;
    margin-bottom: 32px;
  }
  .ghost-card {
    width: 64px;
    aspect-ratio: 5 / 7;
    border-radius: 7px;
    background: var(--field);
    border: 1px solid var(--line);
  }
  h2 {
    margin: 0;
    font-size: 24px;
    line-height: 1.3;
    font-weight: 600;
  }
  .actions {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    margin-top: 28px;
  }
</style>
