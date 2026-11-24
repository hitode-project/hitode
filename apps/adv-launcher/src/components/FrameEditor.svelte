<script lang="ts">
  import { onDestroy } from "svelte";
  import { t } from "../lib/i18n.svelte";
  import { device } from "../lib/device.svelte";
  import {
    ASPECT_PRESETS,
    clampAspect,
    clampZoom,
    contentArea,
    customRect,
    formatAspect,
    formatZoom,
    rectToCustom,
  } from "../lib/custom-frame";
  import { safeBounds } from "../lib/key-layouts";
  import { modal } from "../lib/overlay";
  import type { GameLayout } from "../lib/screen.svelte";
  import { arrange } from "../lib/touch-layout";

  interface Props {
    layout: GameLayout;
    aspect: number;
    zoom: number;
    naturalAspect: number | null;
    onchange: (aspect: number, zoom: number) => void;
    ondone: () => void;
  }

  let { layout, aspect, zoom, naturalAspect, onchange, ondone }: Props = $props();

  const android = $derived(device.platform === "android");
  const area = $derived(contentArea(layout));
  const safe = $derived(safeBounds(layout, layout.width, layout.height, android));

  type Handle = "left" | "right" | "top" | "bottom" | "tl" | "tr" | "bl" | "br";

  interface Drag {
    handle: Handle;
    pointerId: number;
    startW: number;
    startH: number;
    aspect: number;
  }

  let drag = $state<Drag | null>(null);
  let draftAspect = $state(0);
  let draftZoom = $state(0);
  let throttleTimer: ReturnType<typeof setTimeout> | undefined;
  let lastEmit = 0;

  const currentAspect = $derived(drag ? draftAspect : aspect);
  const currentZoom = $derived(drag ? draftZoom : zoom);
  const rect = $derived(customRect(area, currentAspect, currentZoom));
  const arrangement = $derived(arrange({ ...layout, game: rect }, android));

  const fullAspect = $derived(
    area.height > 0 ? clampAspect((area.width / area.height) * 1000) : 1778,
  );

  function flushEmit(nextAspect: number, nextZoom: number, immediate: boolean) {
    if (throttleTimer !== undefined) {
      clearTimeout(throttleTimer);
      throttleTimer = undefined;
    }
    const now = performance.now();
    if (immediate || now - lastEmit >= 50) {
      lastEmit = now;
      onchange(nextAspect, nextZoom);
      return;
    }
    const wait = 50 - (now - lastEmit);
    throttleTimer = setTimeout(() => {
      throttleTimer = undefined;
      lastEmit = performance.now();
      onchange(draftAspect, draftZoom);
    }, wait);
  }

  onDestroy(() => {
    if (throttleTimer !== undefined) clearTimeout(throttleTimer);
  });

  function startDrag(handle: Handle, event: PointerEvent) {
    event.preventDefault();
    event.stopPropagation();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    draftAspect = aspect;
    draftZoom = zoom;
    drag = {
      handle,
      pointerId: event.pointerId,
      startW: rect.width,
      startH: rect.height,
      aspect: rect.height > 0 ? rect.width / rect.height : 4 / 3,
    };
  }

  function moveDrag(event: PointerEvent) {
    if (!drag || event.pointerId !== drag.pointerId) return;
    event.preventDefault();
    const cx = area.x + area.width / 2;
    const cy = area.y + area.height / 2;
    const dx = Math.abs(event.clientX - cx);
    const dy = Math.abs(event.clientY - cy);
    const minW = Math.min(area.width, Math.max(80, area.width * 0.25));
    const minH = Math.min(area.height, Math.max(60, area.height * 0.25));

    let nextW = drag.startW;
    let nextH = drag.startH;
    switch (drag.handle) {
      case "left":
      case "right":
        nextW = Math.max(minW, Math.min(area.width, dx * 2));
        break;
      case "top":
      case "bottom":
        nextH = Math.max(minH, Math.min(area.height, dy * 2));
        break;
      case "tl":
      case "tr":
      case "bl":
      case "br": {
        // Keep the current aspect ratio on corner drags for smooth uniform scaling.
        const r = drag.aspect;
        const maxW = Math.min(area.width, area.height * r);
        const maxH = maxW / r;
        const scale = Math.max(
          (dx * 2) / Math.max(1, maxW),
          (dy * 2) / Math.max(1, maxH),
        );
        const clamped = Math.max(0.5, Math.min(1, scale));
        nextW = maxW * clamped;
        nextH = maxH * clamped;
        break;
      }
    }

    const next = rectToCustom(area, nextW, nextH);
    if (next.aspect !== draftAspect || next.zoom !== draftZoom) {
      draftAspect = next.aspect;
      draftZoom = next.zoom;
      flushEmit(next.aspect, next.zoom, false);
    }
  }

  function endDrag(event: PointerEvent) {
    if (!drag || event.pointerId !== drag.pointerId) return;
    const finalAspect = draftAspect;
    const finalZoom = draftZoom;
    drag = null;
    flushEmit(finalAspect, finalZoom, true);
  }

  function applyPreset(nextAspect: number, nextZoom: number) {
    const a = clampAspect(nextAspect);
    const z = clampZoom(nextZoom);
    draftAspect = a;
    draftZoom = z;
    flushEmit(a, z, true);
  }

  const boxStyle = (a: { x: number; y: number; width: number; height: number }) =>
    `left: ${a.x}px; top: ${a.y}px; width: ${a.width}px; height: ${a.height}px`;
</script>

<div class="editor" role="dialog" aria-modal="true" aria-label={t("调整画面比例")} {@attach modal}>
  <!-- Preview where default touch margins will sit around the edited frame. -->
  {#if arrangement.kind === "side"}
    <div class="margin-hint" style={boxStyle(arrangement.left)} aria-hidden="true"></div>
    <div class="margin-hint" style={boxStyle(arrangement.right)} aria-hidden="true"></div>
  {:else if arrangement.kind === "band"}
    <div class="margin-hint" style={boxStyle(arrangement.top)} aria-hidden="true"></div>
    <div class="margin-hint" style={boxStyle(arrangement.bottom)} aria-hidden="true"></div>
  {/if}

  <!-- Cutout-safe boundary guide. -->
  <div class="safe-frame" style={boxStyle(area)} aria-hidden="true"></div>

  <!-- Live frame outline and symmetric drag handles. -->
  <div class="frame" class:dragging={drag !== null} style={boxStyle(rect)}>
    <div class="edge left" role="slider" tabindex="0" aria-label={t("调整画面宽度")} aria-valuemin={50} aria-valuemax={100} aria-valuenow={Math.round((rect.width / Math.max(1, area.width)) * 100)}
      onpointerdown={(e) => startDrag("left", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}>
      <span></span>
    </div>
    <div class="edge right" role="slider" tabindex="0" aria-label={t("调整画面宽度")} aria-valuemin={50} aria-valuemax={100} aria-valuenow={Math.round((rect.width / Math.max(1, area.width)) * 100)}
      onpointerdown={(e) => startDrag("right", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}>
      <span></span>
    </div>
    <div class="edge top" class:behind-bar={rect.y < safe.y + 52} role="slider" tabindex="0" aria-label={t("调整画面高度")} aria-valuemin={50} aria-valuemax={100} aria-valuenow={Math.round((rect.height / Math.max(1, area.height)) * 100)}
      onpointerdown={(e) => startDrag("top", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}>
      <span></span>
    </div>
    <div class="edge bottom" role="slider" tabindex="0" aria-label={t("调整画面高度")} aria-valuemin={50} aria-valuemax={100} aria-valuenow={Math.round((rect.height / Math.max(1, area.height)) * 100)}
      onpointerdown={(e) => startDrag("bottom", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}>
      <span></span>
    </div>
    <div class="corner tl" aria-hidden="true" onpointerdown={(e) => startDrag("tl", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}></div>
    <div class="corner tr" aria-hidden="true" onpointerdown={(e) => startDrag("tr", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}></div>
    <div class="corner bl" aria-hidden="true" onpointerdown={(e) => startDrag("bl", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}></div>
    <div class="corner br" aria-hidden="true" onpointerdown={(e) => startDrag("br", e)} onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag}></div>
  </div>

  <!-- Compact floating toolbar clear of cutouts and gesture edges. -->
  <div class="bar" class:dragging={drag !== null} style="top: {safe.y + 8}px; left: {safe.x + 8}px; right: {Math.max(8, layout.width - safe.x - safe.width + 8)}px">
    <span class="readout mono">{formatAspect(currentAspect)} · {formatZoom(currentZoom)}</span>
    <div class="presets">
      {#if naturalAspect !== null}
        <button type="button" class="pill" class:on={Math.abs(currentAspect - naturalAspect) <= 12 && currentZoom === 1000}
          onclick={() => applyPreset(naturalAspect, 1000)}>{t("原比例")}</button>
      {/if}
      {#each ASPECT_PRESETS as p (p.label)}
        <button type="button" class="pill" class:on={Math.abs(currentAspect - p.aspect) <= 12}
          onclick={() => applyPreset(p.aspect, currentZoom)}>{p.label}</button>
      {/each}
      <button type="button" class="pill" class:on={Math.abs(currentAspect - fullAspect) <= 12 && currentZoom === 1000}
        onclick={() => applyPreset(fullAspect, 1000)}>{t("满屏")}</button>
    </div>
    <button type="button" class="done" onclick={ondone}>{t("完成")}</button>
  </div>
</div>

<style>
  .editor {
    position: absolute;
    inset: 0;
    user-select: none;
    -webkit-user-select: none;
    touch-action: none;
    color: #ececea;
    color-scheme: dark;
  }
  .safe-frame {
    position: absolute;
    box-sizing: border-box;
    border: 1px dashed rgba(255, 255, 255, 0.16);
    pointer-events: none;
  }
  .margin-hint {
    position: absolute;
    box-sizing: border-box;
    border-radius: 10px;
    border: 1px dashed rgba(143, 211, 166, 0.35);
    background: rgba(143, 211, 166, 0.06);
    pointer-events: none;
  }
  .frame {
    position: absolute;
    box-sizing: border-box;
    border: 2px solid var(--accent, #3767d6);
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.55), 0 0 0 9999px rgba(0, 0, 0, 0.32);
    pointer-events: none;
  }
  .frame.dragging {
    border-color: #ffffff;
  }
  .edge {
    position: absolute;
    pointer-events: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    touch-action: none;
  }
  .edge span {
    border-radius: 999px;
    background: #ffffff;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.6);
  }
  .edge.left,
  .edge.right {
    top: 36px;
    bottom: 36px;
    width: 36px;
    cursor: ew-resize;
  }
  .edge.left {
    left: 0;
    justify-content: flex-start;
    padding-left: 5px;
  }
  .edge.right {
    right: 0;
    justify-content: flex-end;
    padding-right: 5px;
  }
  .edge.left span,
  .edge.right span {
    width: 4px;
    height: 44px;
  }
  .edge.top,
  .edge.bottom {
    left: 36px;
    right: 36px;
    height: 36px;
    cursor: ns-resize;
  }
  .edge.top {
    top: 0;
    align-items: flex-start;
    padding-top: 5px;
  }
  .edge.top.behind-bar span {
    opacity: 0;
  }
  .edge.bottom {
    bottom: 0;
    align-items: flex-end;
    padding-bottom: 5px;
  }
  .edge.top span,
  .edge.bottom span {
    width: 44px;
    height: 4px;
  }
  .corner {
    position: absolute;
    width: 36px;
    height: 36px;
    pointer-events: auto;
    touch-action: none;
  }
  .corner::after {
    content: "";
    position: absolute;
    width: 14px;
    height: 14px;
    border-radius: 999px;
    background: #ffffff;
    border: 2px solid var(--accent, #3767d6);
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.6);
  }
  .corner.tl {
    top: 0;
    left: 0;
    cursor: nwse-resize;
  }
  .corner.tl::after {
    top: 4px;
    left: 4px;
  }
  .corner.tr {
    top: 0;
    right: 0;
    cursor: nesw-resize;
  }
  .corner.tr::after {
    top: 4px;
    right: 4px;
  }
  .corner.bl {
    bottom: 0;
    left: 0;
    cursor: nesw-resize;
  }
  .corner.bl::after {
    bottom: 4px;
    left: 4px;
  }
  .corner.br {
    bottom: 0;
    right: 0;
    cursor: nwse-resize;
  }
  .corner.br::after {
    bottom: 4px;
    right: 4px;
  }
  .bar {
    position: absolute;
    margin: 0 auto;
    max-width: max-content;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    justify-content: center;
    padding: 6px 8px 6px 12px;
    border-radius: 14px;
    background: rgba(20, 20, 24, 0.45);
    backdrop-filter: blur(12px) saturate(140%);
    -webkit-backdrop-filter: blur(12px) saturate(140%);
    border: 1px solid rgba(255, 255, 255, 0.16);
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.28);
    transition: opacity 0.15s ease;
  }
  .bar.dragging {
    opacity: 0.45;
    pointer-events: none;
  }
  .readout {
    font-size: 12px;
    color: rgba(244, 244, 242, 0.92);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.45);
    white-space: nowrap;
  }
  .presets {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .pill {
    height: 30px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.14);
    background: rgba(255, 255, 255, 0.1);
    color: #f4f4f2;
    font-size: 12px;
    white-space: nowrap;
    cursor: pointer;
  }
  .pill.on {
    background: rgba(255, 255, 255, 0.26);
    border-color: rgba(255, 255, 255, 0.42);
    font-weight: 500;
  }
  .done {
    height: 30px;
    padding: 0 14px;
    border-radius: 8px;
    border: 0;
    background: var(--accent, #3767d6);
    color: #ffffff;
    font-size: 13px;
    font-weight: 500;
    white-space: nowrap;
    cursor: pointer;
  }
</style>
