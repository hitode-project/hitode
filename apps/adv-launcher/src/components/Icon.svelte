<script lang="ts">
  import { icons, type IconName } from "../lib/icons";

  interface Props {
    name: IconName;
    size?: number;
    stroke?: number;
  }

  let { name, size = 16, stroke = 1.4 }: Props = $props();

  const raw = $derived<string>(icons[name]);
  const filled = $derived(raw.startsWith("fill:"));
  const body = $derived(filled ? raw.slice(5) : raw);
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill={filled ? "currentColor" : "none"}
  stroke={filled ? "none" : "currentColor"}
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <!-- Static markup from lib/icons.ts, never user data. -->
  {@html body}
</svg>
