<script lang="ts">
  interface Props {
    checked: boolean;
    label: string;
    onchange: (checked: boolean) => void;
  }

  let { checked, label, onchange }: Props = $props();
</script>

<button class="sw" class:on={checked} role="switch" aria-checked={checked} aria-label={label} onclick={() => onchange(!checked)}></button>

<style>
  .sw {
    --off: rgba(24, 24, 22, 0.2);
    --knob: #ffffff;
    all: unset;
    box-sizing: border-box;
    position: relative;
    flex: 0 0 auto;
    width: 44px;
    height: 26px;
    border-radius: 13px;
    background: var(--off);
    cursor: pointer;
    transition: background 0.15s ease;
  }
  :global(:root[data-theme="dark"]) .sw {
    --off: rgba(255, 255, 255, 0.2);
    --knob: #ececea;
  }
  .sw::after {
    content: "";
    position: absolute;
    top: 3px;
    left: 3px;
    width: 20px;
    height: 20px;
    border-radius: 10px;
    background: var(--knob);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
    transition: transform 0.15s ease;
  }
  .sw.on {
    background: var(--accent);
  }
  .sw.on::after {
    transform: translateX(18px);
    background: #ffffff;
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
