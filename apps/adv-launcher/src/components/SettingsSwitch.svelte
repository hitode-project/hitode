<script lang="ts">
  interface Props {
    label: string;
    checked: boolean;
    onchange: (checked: boolean) => void;
  }

  let { label, checked, onchange }: Props = $props();
</script>

<button class="sw" class:on={checked} role="switch" aria-checked={checked} aria-label={label} onclick={() => onchange(!checked)}></button>

<style>
  .sw {
    all: unset;
    box-sizing: border-box;
    position: relative;
    flex: 0 0 auto;
    width: 44px;
    height: 26px;
    border-radius: 13px;
    background: rgba(24, 24, 22, 0.2);
    cursor: pointer;
    transition: background 0.15s ease;
  }
  :global(:root[data-theme="dark"]) .sw {
    background: rgba(255, 255, 255, 0.2);
  }
  .sw::after {
    content: "";
    position: absolute;
    top: 3px;
    left: 3px;
    width: 20px;
    height: 20px;
    border-radius: 10px;
    background: #ffffff;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
    transition: transform 0.15s ease;
  }
  :global(:root[data-theme="dark"]) .sw::after {
    background: #ececea;
  }
  .sw.on,
  :global(:root[data-theme="dark"]) .sw.on {
    background: var(--accent);
  }
  .sw.on::after,
  :global(:root[data-theme="dark"]) .sw.on::after {
    transform: translateX(18px);
    background: var(--on-accent);
  }
  .sw:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .sw,
    .sw::after {
      transition: none;
    }
  }
</style>
