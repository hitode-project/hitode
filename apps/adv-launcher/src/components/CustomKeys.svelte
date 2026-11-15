<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { call } from "../lib/bridge";
  import { device } from "../lib/device.svelte";
  import { t } from "../lib/i18n.svelte";
  import {
     clampOpacity,
    clampScale,
    bindingCanLatch,
    bindingEquals,
    dpadDirections,
    dpadVkMap,
    IDLE_DPAD,
    itemCenter,
    KEY_CATALOG,
    mouseButtonOf,
    pointToNormalized,
    safeBounds,
    type DpadItem,
    type DpadState,
    type KeyBinding,
    type KeyCategory,
    type KeyItem,
    type KeyProfile,
    type OverlayItem,
  } from "../lib/key-layouts";
  import {
    deleteKeyProfile,
    forkKeyProfile,
    keyProfiles,
    saveKeyLayouts,
    setGameKeyProfile,
  } from "../lib/key-layouts.svelte";
  import { modal, region } from "../lib/overlay";
  import type { GameLayout } from "../lib/screen.svelte";
  import { tap } from "../lib/tap";

  interface Props {
    gameId: string;
    profile: KeyProfile;
    layout: GameLayout | null;
    editing: boolean;
    latchCtrl: boolean;
    onmenu: () => void;
    onkeyboard: () => void;
    ondone: () => void;
  }

  let { gameId, profile, layout, editing, latchCtrl, onmenu, onkeyboard, ondone }: Props = $props();

  const android = $derived(device.platform === "android");
  let vw = $state(typeof window !== "undefined" ? window.innerWidth : 844);
  let vh = $state(typeof window !== "undefined" ? window.innerHeight : 390);

  const bounds = $derived(safeBounds(layout, layout?.width ?? vw, layout?.height ?? vh, android));

  // Play-mode transient states
  let tempCollapsed = $state(false);
  let idleDim = $state(false);
  let idleTimer: ReturnType<typeof setTimeout> | undefined;

  interface DpadKnob {
    x: number;
    y: number;
    active: boolean;
  }
  const IDLE_KNOB: DpadKnob = { x: 0, y: 0, active: false };

  let heldCounts = $state<Record<string, number>>({});
  let latchedIds = $state<Record<string, boolean>>({});
  let dpadStates = $state<Record<string, DpadState>>({});
  let dpadKnobs = $state<Record<string, DpadKnob>>({});
  const pointerItem = new Map<number, string>();
  const dpadPointer = new Map<string, number>();

  // Edit-mode UI states
  let selectedId = $state<string | null>(null);
  let activeTab = $state<KeyCategory>("common");
  let kgridEl = $state<HTMLDivElement | null>(null);
  let topPopover = $state<"profiles" | "opacity" | null>(null);
  let forkOpen = $state(false);
  let forkName = $state("");
  let guideX = $state<number | null>(null);
  let guideY = $state<number | null>(null);

  interface DragSession {
    itemId: string;
    pointerId: number;
    startX: number;
    startY: number;
    origCx: number;
    origCy: number;
    moved: boolean;
  }
  let drag = $state<DragSession | null>(null);

  const selectedItem = $derived(
    selectedId ? (profile.items.find((it) => it.id === selectedId) ?? null) : null,
  );

  function profileLabel(p: KeyProfile): string {
    if (p.builtin && p.name === "快捷键") return t("快捷键");
    return p.name;
  }

  function bindingLabel(b: KeyBinding): string {
    if (b.kind === "sys" || b.kind === "mouse" || b.kind === "wheel" || b.label === "退格") {
      return t(b.label);
    }
    return b.label;
  }

  function haptic() {
    void call("haptic_tick", {}, null).catch(() => {});
  }

  function sendBindingEdge(binding: KeyBinding, down: boolean) {
    if (binding.kind === "vk") {
      void call("game_key", { key: binding.vk, down }, null).catch(() => {});
    } else if (binding.kind === "mouse") {
      const btn = mouseButtonOf(binding);
      const button = btn === "middle" ? 2 : btn === "right" ? 1 : 0;
      void call("game_mouse_button", { right: btn === "right", button, down }, null).catch(() => {});
    }
  }

  function releaseAllActive() {
    for (const item of profile.items) {
      if (item.type === "key") {
        if ((heldCounts[item.id] ?? 0) > 0 || latchedIds[item.id]) {
          sendBindingEdge(item.binding, false);
        }
      } else {
        const st = dpadStates[item.id];
        if (st) {
          const vks = dpadVkMap(item.bind);
          if (st.up) void call("game_key", { key: vks.up, down: false }, null).catch(() => {});
          if (st.down) void call("game_key", { key: vks.down, down: false }, null).catch(() => {});
          if (st.left) void call("game_key", { key: vks.left, down: false }, null).catch(() => {});
          if (st.right) void call("game_key", { key: vks.right, down: false }, null).catch(() => {});
        }
      }
    }
    pointerItem.clear();
    dpadPointer.clear();
    if (Object.keys(heldCounts).length) heldCounts = {};
    if (Object.keys(latchedIds).length) latchedIds = {};
    if (Object.keys(dpadStates).length) dpadStates = {};
    if (Object.keys(dpadKnobs).length) dpadKnobs = {};
  }

  const anyActiveInput = $derived(
    Object.values(heldCounts).some((n) => n > 0) ||
      Object.values(latchedIds).some(Boolean) ||
      Object.values(dpadStates).some((s) => s.up || s.down || s.left || s.right) ||
      Object.values(dpadKnobs).some((k) => k.active),
  );

  function bumpActivity() {
    idleDim = false;
    if (idleTimer !== undefined) {
      clearTimeout(idleTimer);
      idleTimer = undefined;
    }
    if (!editing && profile.autoDim && !tempCollapsed && !anyActiveInput) {
      idleTimer = setTimeout(() => {
        idleTimer = undefined;
        if (!editing && profile.autoDim && !anyActiveInput) {
          idleDim = true;
        }
      }, 2500);
    }
  }

  $effect(() => {
    const _pid = profile.id;
    const _edit = editing;
    releaseAllActive();
    if (_edit) {
      tempCollapsed = false;
      idleDim = false;
      if (idleTimer !== undefined) {
        clearTimeout(idleTimer);
        idleTimer = undefined;
      }
    } else {
      selectedId = null;
      topPopover = null;
      forkOpen = false;
      guideX = null;
      guideY = null;
      bumpActivity();
    }
    void _pid;
  });

  $effect(() => {
    if (anyActiveInput || !profile.autoDim || editing || tempCollapsed) {
      idleDim = false;
      if (idleTimer !== undefined) {
        clearTimeout(idleTimer);
        idleTimer = undefined;
      }
    } else {
      bumpActivity();
    }
  });

  onDestroy(() => {
    if (idleTimer !== undefined) clearTimeout(idleTimer);
    releaseAllActive();
  });

  function inAndroidGestureStrip(event: PointerEvent): boolean {
    if (!android || !layout || event.pointerType !== "touch") return false;
    const { gestures, width, height } = layout;
    return (
      event.clientX < gestures.left ||
      event.clientX >= width - gestures.right ||
      event.clientY < gestures.top ||
      event.clientY >= height - gestures.bottom
    );
  }

  function effectiveLatch(item: KeyItem): boolean {
    return (
      bindingCanLatch(item.binding) &&
      (item.latch || (item.binding.kind === "vk" && item.binding.vk === 0x11 && latchCtrl))
    );
  }

  function triggerInstantOrToggle(item: KeyItem) {
    bumpActivity();
    const b = item.binding;
    if (b.kind === "sys") {
      haptic();
      if (b.sys === "menu") {
        releaseAllActive();
        onmenu();
      } else if (b.sys === "kbd") {
        releaseAllActive();
        onkeyboard();
      } else if (b.sys === "hide") {
        const next = !tempCollapsed;
        if (next) releaseAllActive();
        tempCollapsed = next;
      }
      return;
    }
    if (b.kind === "wheel") {
      haptic();
      void call("game_wheel", { notches: b.notches }, null).catch(() => {});
      return;
    }
    if (effectiveLatch(item)) {
      haptic();
      const next = !latchedIds[item.id];
      latchedIds = { ...latchedIds, [item.id]: next };
      sendBindingEdge(b, next);
      return;
    }
    // Short pulse for an edge-strip completed tap on Android (AND-APP-18).
    haptic();
    sendBindingEdge(b, true);
    setTimeout(() => sendBindingEdge(b, false), 90);
  }

  function playKeyHandlers(item: KeyItem) {
    let edge = false;
    const edgeTap = tap(() => triggerInstantOrToggle(item));

    function lift(pointerId: number) {
      if (pointerItem.get(pointerId) !== item.id) return;
      pointerItem.delete(pointerId);
      const prev = heldCounts[item.id] ?? 0;
      const next = Math.max(0, prev - 1);
      heldCounts = { ...heldCounts, [item.id]: next };
      if (prev > 0 && next === 0) {
        sendBindingEdge(item.binding, false);
      }
      bumpActivity();
    }

    return {
      onpointerdown(event: PointerEvent) {
        if (editing) return;
        bumpActivity();
        edge = inAndroidGestureStrip(event);
        if (edge) {
          edgeTap.onpointerdown(event);
          return;
        }
        const b = item.binding;
        if (b.kind === "sys" || b.kind === "wheel" || effectiveLatch(item)) {
          event.preventDefault();
          triggerInstantOrToggle(item);
          return;
        }
        event.preventDefault();
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        pointerItem.set(event.pointerId, item.id);
        const prev = heldCounts[item.id] ?? 0;
        heldCounts = { ...heldCounts, [item.id]: prev + 1 };
        if (prev === 0) {
          haptic();
          sendBindingEdge(b, true);
        }
      },
      onpointermove(event: PointerEvent) {
        if (editing) return;
        if (edge) edgeTap.onpointermove(event);
      },
      onpointerup(event: PointerEvent) {
        if (editing) return;
        if (edge) edgeTap.onpointerup(event);
        lift(event.pointerId);
      },
      onpointercancel(event: PointerEvent) {
        if (editing) return;
        if (edge) edgeTap.onpointercancel(event);
        lift(event.pointerId);
      },
      onclick(event: MouseEvent) {
        if (editing) return;
        if (edge) edgeTap.onclick(event);
      },
      onlostpointercapture(event: PointerEvent) {
        if (!editing) lift(event.pointerId);
      },
      oncontextmenu(event: Event) {
        event.preventDefault();
      },
    };
  }

  function applyDpadVector(item: DpadItem, clientX: number, clientY: number) {
    const { cx, cy, width } = itemCenter(item, bounds);
    const dx = clientX - cx;
    const dy = clientY - cy;
    const maxTravel = Math.max(16, width * 0.22);
    const dist = Math.hypot(dx, dy);
    const kx = dist > maxTravel ? (dx / dist) * maxTravel : dx;
    const ky = dist > maxTravel ? (dy / dist) * maxTravel : dy;
    dpadKnobs = { ...dpadKnobs, [item.id]: { x: kx, y: ky, active: true } };

    const deadzone = Math.max(10, Math.round(13 * item.scale));
    const next = dpadDirections(dx, dy, deadzone, item.eightWay);
    const prev = dpadStates[item.id] ?? IDLE_DPAD;
    if (
      prev.up === next.up &&
      prev.down === next.down &&
      prev.left === next.left &&
      prev.right === next.right
    ) {
      return;
    }
    const vks = dpadVkMap(item.bind);
    if (prev.up !== next.up) void call("game_key", { key: vks.up, down: next.up }, null).catch(() => {});
    if (prev.down !== next.down) void call("game_key", { key: vks.down, down: next.down }, null).catch(() => {});
    if (prev.left !== next.left) void call("game_key", { key: vks.left, down: next.left }, null).catch(() => {});
    if (prev.right !== next.right) void call("game_key", { key: vks.right, down: next.right }, null).catch(() => {});
    if (next.up || next.down || next.left || next.right) haptic();
    dpadStates = { ...dpadStates, [item.id]: next };
  }

  function releaseDpad(item: DpadItem) {
    dpadPointer.delete(item.id);
    const prev = dpadStates[item.id] ?? IDLE_DPAD;
    const vks = dpadVkMap(item.bind);
    if (prev.up) void call("game_key", { key: vks.up, down: false }, null).catch(() => {});
    if (prev.down) void call("game_key", { key: vks.down, down: false }, null).catch(() => {});
    if (prev.left) void call("game_key", { key: vks.left, down: false }, null).catch(() => {});
    if (prev.right) void call("game_key", { key: vks.right, down: false }, null).catch(() => {});
    dpadStates = { ...dpadStates, [item.id]: IDLE_DPAD };
    dpadKnobs = { ...dpadKnobs, [item.id]: IDLE_KNOB };
    bumpActivity();
  }

  function playDpadHandlers(item: DpadItem) {
    return {
      onpointerdown(event: PointerEvent) {
        if (editing || dpadPointer.has(item.id)) return;
        event.preventDefault();
        bumpActivity();
        dpadPointer.set(item.id, event.pointerId);
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        applyDpadVector(item, event.clientX, event.clientY);
      },
      onpointermove(event: PointerEvent) {
        if (editing || dpadPointer.get(item.id) !== event.pointerId) return;
        applyDpadVector(item, event.clientX, event.clientY);
      },
      onpointerup(event: PointerEvent) {
        if (editing || dpadPointer.get(item.id) !== event.pointerId) return;
        releaseDpad(item);
      },
      onpointercancel(event: PointerEvent) {
        if (editing || dpadPointer.get(item.id) !== event.pointerId) return;
        releaseDpad(item);
      },
      onlostpointercapture(event: PointerEvent) {
        if (!editing && dpadPointer.get(item.id) === event.pointerId) releaseDpad(item);
      },
      oncontextmenu(event: Event) {
        event.preventDefault();
      },
    };
  }

  // Edit-mode drag & selection handlers
  function pickTab(cat: KeyCategory) {
    activeTab = cat;
    if (kgridEl) kgridEl.scrollTop = 0;
  }

  function selectItemForEdit(item: OverlayItem) {
    selectedId = item.id;
    topPopover = null;
    if (item.type === "key") {
      for (const cat of ["common", "alpha", "symbol", "fn", "numpad", "system"] as const) {
        if (KEY_CATALOG[cat].some((b) => bindingEquals(b, item.binding))) {
          activeTab = cat;
          break;
        }
      }
      void tick().then(() => {
        kgridEl?.querySelector<HTMLElement>(".kchip.on")?.scrollIntoView({ block: "nearest" });
      });
    }
  }

  function editItemHandlers(item: OverlayItem) {
    return {
      onpointerdown(event: PointerEvent) {
        if (!editing) return;
        event.stopPropagation();
        event.preventDefault();
        topPopover = null;
        const { cx, cy } = itemCenter(item, bounds);
        drag = {
          itemId: item.id,
          pointerId: event.pointerId,
          startX: event.clientX,
          startY: event.clientY,
          origCx: cx,
          origCy: cy,
          moved: false,
        };
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
      },
      onpointermove(event: PointerEvent) {
        if (!editing || !drag || drag.itemId !== item.id || drag.pointerId !== event.pointerId) return;
        const dx = event.clientX - drag.startX;
        const dy = event.clientY - drag.startY;
        if (!drag.moved && Math.hypot(dx, dy) < 4) return;
        drag.moved = true;
        selectedId = item.id;

        let nextCx = drag.origCx + dx;
        let nextCy = drag.origCy + dy;
        let gx: number | null = null;
        let gy: number | null = null;
        const SNAP = 5;

        const midX = bounds.x + bounds.width / 2;
        const midY = bounds.y + bounds.height / 2;
        if (Math.abs(nextCx - midX) <= SNAP) {
          nextCx = midX;
          gx = midX;
        }
        if (Math.abs(nextCy - midY) <= SNAP) {
          nextCy = midY;
          gy = midY;
        }

        for (const other of profile.items) {
          if (other.id === item.id) continue;
          const oc = itemCenter(other, bounds);
          if (gx === null && Math.abs(nextCx - oc.cx) <= SNAP) {
            nextCx = oc.cx;
            gx = oc.cx;
          }
          if (gy === null && Math.abs(nextCy - oc.cy) <= SNAP) {
            nextCy = oc.cy;
            gy = oc.cy;
          }
        }

        guideX = gx;
        guideY = gy;
        const norm = pointToNormalized(nextCx, nextCy, item, bounds);
        item.rx = norm.rx;
        item.ry = norm.ry;
      },
      onpointerup(event: PointerEvent) {
        if (!editing || !drag || drag.itemId !== item.id || drag.pointerId !== event.pointerId) return;
        const wasMoved = drag.moved;
        drag = null;
        guideX = null;
        guideY = null;
        if (wasMoved) {
          saveKeyLayouts();
        } else {
          selectItemForEdit(item);
        }
      },
      onpointercancel(event: PointerEvent) {
        if (!editing || !drag || drag.itemId !== item.id || drag.pointerId !== event.pointerId) return;
        drag = null;
        guideX = null;
        guideY = null;
        saveKeyLayouts();
      },
      oncontextmenu(event: Event) {
        event.preventDefault();
      },
    };
  }

  function freeSpot(preferLeft: boolean): { rx: number; ry: number } {
    const baseRx = preferLeft ? 0.22 : 0.5;
    const candidates = [
      { rx: baseRx, ry: 0.52 },
      { rx: baseRx + 0.1, ry: 0.52 },
      { rx: baseRx - 0.08, ry: 0.64 },
      { rx: 0.5, ry: 0.66 },
      { rx: 0.62, ry: 0.52 },
    ];
    for (const c of candidates) {
      const clash = profile.items.some(
        (it) => Math.hypot(it.rx - c.rx, it.ry - c.ry) < 0.08,
      );
      if (!clash) return c;
    }
    return { rx: 0.5, ry: 0.55 };
  }

  function addKey() {
    const spot = freeSpot(false);
    const id = `k-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 5)}`;
    const next: KeyItem = {
      id,
      type: "key",
      rx: spot.rx,
      ry: spot.ry,
      scale: 1,
      latch: false,
      binding: { kind: "vk", vk: 0x20, label: "Space" },
    };
    profile.items.push(next);
    saveKeyLayouts();
    selectItemForEdit(next);
  }

  function addDpad() {
    const existing = profile.items.find((it) => it.type === "dpad");
    if (existing) {
      selectItemForEdit(existing);
      return;
    }
    const spot = freeSpot(true);
    const id = `d-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 5)}`;
    const next: DpadItem = {
      id,
      type: "dpad",
      rx: spot.rx,
      ry: spot.ry,
      scale: 1,
      bind: "arrows",
      eightWay: true,
    };
    profile.items.push(next);
    saveKeyLayouts();
    selectItemForEdit(next);
  }

  function removeSelected() {
    if (!selectedId) return;
    profile.items = profile.items.filter((it) => it.id !== selectedId);
    selectedId = null;
    saveKeyLayouts();
  }

  function applyScaleToPeers(source: OverlayItem) {
    const s = clampScale(source.scale);
    for (const it of profile.items) {
      if (it.type === source.type) it.scale = s;
    }
    saveKeyLayouts();
    haptic();
  }

  function commitFork() {
    const created = forkKeyProfile(profile.id, forkName || `${profileLabel(profile)} 2`, gameId);
    forkName = "";
    forkOpen = false;
    topPopover = null;
    selectedId = null;
    void created;
  }

  const gridMaxHeight = $derived(Math.max(108, Math.min(156, (layout?.height ?? vh) - 204)));

  const inspectorStyle = $derived.by(() => {
    if (!editing || !selectedItem || drag?.moved) return "";
    const { cx, cy, width, height } = itemCenter(selectedItem, bounds);
    const screenW = layout?.width ?? vw;
    const screenH = layout?.height ?? vh;
    const popW = Math.min(selectedItem.type === "key" ? 384 : 328, Math.max(240, screenW - 20));
    const popH = selectedItem.type === "key" ? gridMaxHeight + 140 : 138;
    const gap = 10;
    const minTop = Math.max(54, bounds.y + 46);

    // Prefer placing beside the item horizontally when vertical room is tight on landscape phones.
    const roomRight = screenW - (cx + width / 2 + gap) - 10;
    const roomLeft = cx - width / 2 - gap - 10;
    let left: number;
    let top: number;

    if (roomRight >= popW) {
      left = cx + width / 2 + gap;
      top = Math.min(screenH - popH - 8, Math.max(minTop, cy - popH / 2));
    } else if (roomLeft >= popW) {
      left = cx - width / 2 - gap - popW;
      top = Math.min(screenH - popH - 8, Math.max(minTop, cy - popH / 2));
    } else {
      left = Math.min(screenW - popW - 10, Math.max(10, cx - popW / 2));
      const preferAbove = cy > screenH * 0.54;
      top = preferAbove
        ? Math.max(minTop, cy - height / 2 - gap - popH)
        : Math.min(screenH - popH - 8, cy + height / 2 + gap);
    }
    return `left: ${Math.round(left)}px; top: ${Math.round(Math.max(8, top))}px; width: ${popW}px`;
  });

  const layerOpacity = $derived(
    editing
      ? profile.opacity
      : idleDim
        ? Math.min(profile.opacity, 0.24)
        : profile.opacity,
  );

  const visibleItems = $derived(
    !editing && tempCollapsed
      ? profile.items.filter(
          (it): it is KeyItem =>
            it.type === "key" && it.binding.kind === "sys" && it.binding.sys === "hide",
        )
      : profile.items,
  );
</script>

<svelte:window
  onresize={() => {
    vw = window.innerWidth;
    vh = window.innerHeight;
  }}
/>

{#snippet keyFace(item: KeyItem)}
  {#if item.binding.kind === "sys" && item.binding.sys === "hide"}
    <Icon name={tempCollapsed && !editing ? "eye-off" : "eye"} size={Math.round(16 * Math.min(1.25, Math.max(0.85, item.scale)))} />
  {:else if item.binding.kind === "sys" && item.binding.sys === "menu"}
    <Icon name="menu" size={Math.round(16 * Math.min(1.25, Math.max(0.85, item.scale)))} />
  {:else if item.binding.kind === "sys" && item.binding.sys === "kbd"}
    <Icon name="keyboard" size={Math.round(16 * Math.min(1.25, Math.max(0.85, item.scale)))} />
  {:else}
    <span style="font-size: {Math.round(12.5 * Math.min(1.28, Math.max(0.82, item.scale)))}px">
      {bindingLabel(item.binding)}
    </span>
  {/if}
{/snippet}

{#snippet dpadFace(item: DpadItem, st: DpadState, knob: DpadKnob)}
  {@const labels = item.bind === "wasd" ? ["W", "D", "S", "A"] : item.bind === "numpad" ? ["8", "6", "2", "4"] : null}
  <svg viewBox="0 0 120 120" width="100%" height="100%" aria-hidden="true">
    <circle cx="60" cy="60" r="56" class="dp-ring" />
    {#if item.eightWay}
      <line x1="29" y1="29" x2="35" y2="35" class="dp-diag" class:on={st.up && st.left} />
      <line x1="91" y1="29" x2="85" y2="35" class="dp-diag" class:on={st.up && st.right} />
      <line x1="91" y1="91" x2="85" y2="85" class="dp-diag" class:on={st.down && st.right} />
      <line x1="29" y1="91" x2="35" y2="85" class="dp-diag" class:on={st.down && st.left} />
    {/if}
    <g class="dp-dir" class:on={st.up}>
      {#if labels}
        <text x="60" y="23" text-anchor="middle" dominant-baseline="central">{labels[0]}</text>
      {:else}
        <path d="M53 25L60 17L67 25" />
      {/if}
    </g>
    <g class="dp-dir" class:on={st.right}>
      {#if labels}
        <text x="97" y="60" text-anchor="middle" dominant-baseline="central">{labels[1]}</text>
      {:else}
        <path d="M95 53L103 60L95 67" />
      {/if}
    </g>
    <g class="dp-dir" class:on={st.down}>
      {#if labels}
        <text x="60" y="97" text-anchor="middle" dominant-baseline="central">{labels[2]}</text>
      {:else}
        <path d="M53 95L60 103L67 95" />
      {/if}
    </g>
    <g class="dp-dir" class:on={st.left}>
      {#if labels}
        <text x="23" y="60" text-anchor="middle" dominant-baseline="central">{labels[3]}</text>
      {:else}
        <path d="M25 53L17 60L25 67" />
      {/if}
    </g>
  </svg>
  <span
    class="dp-puck"
    class:active={knob.active}
    style="transform: translate3d(calc(-50% + {knob.x.toFixed(1)}px), calc(-50% + {knob.y.toFixed(1)}px), 0)"
    aria-hidden="true"
  ></span>
{/snippet}

{#if editing}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="edit-layer fade"
    {@attach modal}
    onpointerdown={() => {
      selectedId = null;
      topPopover = null;
      forkOpen = false;
    }}
  >
    <div
      class="safe-box"
      style="left: {bounds.x}px; top: {bounds.y}px; width: {bounds.width}px; height: {bounds.height}px"
      aria-hidden="true"
    ></div>

    {#if guideX !== null}
      <div class="guide-v" style="left: {guideX}px" aria-hidden="true"></div>
    {/if}
    {#if guideY !== null}
      <div class="guide-h" style="top: {guideY}px" aria-hidden="true"></div>
    {/if}

    <div class="items-layer" style="opacity: {layerOpacity}">
      {#each profile.items as item (item.id)}
        {@const box = itemCenter(item, bounds)}
        {#if item.type === "key"}
          <button
            type="button"
            class="vkey edit-item"
            class:round={item.binding.kind === "sys"}
            class:selected={selectedId === item.id}
            style="left: {box.cx - box.width / 2}px; top: {box.cy - box.height / 2}px; width: {box.width}px; height: {box.height}px"
            aria-label={bindingLabel(item.binding)}
            {...editItemHandlers(item)}
          >
            {@render keyFace(item)}
            {#if effectiveLatch(item)}<span class="latch-dot" aria-hidden="true"></span>{/if}
          </button>
        {:else}
          <div
            role="button"
            tabindex="0"
            class="dpad edit-item"
            class:selected={selectedId === item.id}
            style="left: {box.cx - box.width / 2}px; top: {box.cy - box.height / 2}px; width: {box.width}px; height: {box.height}px"
            aria-label={t("方向盘")}
            {...editItemHandlers(item)}
          >
            {@render dpadFace(item, IDLE_DPAD, IDLE_KNOB)}
          </div>
        {/if}
      {/each}
    </div>

    <div
      class="toolbar"
      style="top: {Math.max(8, bounds.y)}px"
      onpointerdown={(e) => e.stopPropagation()}
    >
      <button
        type="button"
        class="tbtn"
        class:on={topPopover === "profiles"}
        onclick={() => {
          selectedId = null;
          forkOpen = false;
          topPopover = topPopover === "profiles" ? null : "profiles";
        }}
      >
        <span>{profileLabel(profile)}</span>
        <Icon name="chevron-down" size={13} />
      </button>
      <div class="tsep"></div>
      <button type="button" class="tbtn" onclick={addKey}>
        <Icon name="plus" size={13} />
        <span>{t("按键")}</span>
      </button>
      <button type="button" class="tbtn" onclick={addDpad}>
        <Icon name="plus" size={13} />
        <span>{t("方向盘")}</span>
      </button>
      <button
        type="button"
        class="tbtn"
        class:on={topPopover === "opacity"}
        onclick={() => {
          selectedId = null;
          topPopover = topPopover === "opacity" ? null : "opacity";
        }}
      >
        <span>{t("透明度")}</span>
      </button>
      <div class="tsep"></div>
      <button type="button" class="tdone" onclick={ondone}>{t("完成")}</button>
    </div>

    {#if topPopover === "profiles"}
      <div
        class="popover top-pop"
        style="top: {Math.max(8, bounds.y) + 44}px"
        onpointerdown={(e) => e.stopPropagation()}
      >
        <div class="plist">
          <div class="pitem">
            <button
              type="button"
              class="pname"
              onclick={() => {
                setGameKeyProfile(gameId, null);
                topPopover = null;
                selectedId = null;
                ondone();
              }}
            >
              <span>{t("默认")}</span>
            </button>
          </div>
          {#each keyProfiles() as p (p.id)}
            <div class="pitem" class:active={p.id === profile.id}>
              <button
                type="button"
                class="pname"
                onclick={() => {
                  setGameKeyProfile(gameId, p.id);
                  topPopover = null;
                  selectedId = null;
                }}
              >
                <span>{profileLabel(p)}</span>
                {#if p.id === profile.id}<Icon name="check" size={14} />{/if}
              </button>
              {#if !p.builtin}
                <button
                  type="button"
                  class="pdel"
                  aria-label={t("删除")}
                  onclick={() => deleteKeyProfile(p.id)}
                >
                  <Icon name="trash" size={13} />
                </button>
              {/if}
            </div>
          {/each}
        </div>
        <div class="psep"></div>
        {#if forkOpen}
          <div class="fork-row">
            <input
              type="text"
              class="fork-input"
              placeholder={t("方案名称")}
              maxlength="24"
              bind:value={forkName}
              onkeydown={(e) => {
                if (e.key === "Enter" && !e.isComposing) {
                  e.preventDefault();
                  commitFork();
                }
              }}
            />
            <button type="button" class="pill-action" onclick={commitFork}>{t("保存")}</button>
          </div>
        {:else}
          <button
            type="button"
            class="fork-btn"
            onclick={() => {
              forkName = `${profileLabel(profile)} 2`;
              forkOpen = true;
            }}
          >
            <Icon name="plus" size={13} />
            <span>{t("另存")}</span>
          </button>
        {/if}
      </div>
    {:else if topPopover === "opacity"}
      <div
        class="popover top-pop opacity-pop"
        style="top: {Math.max(8, bounds.y) + 44}px"
        onpointerdown={(e) => e.stopPropagation()}
      >
        <div class=" mrow">
          <span>{t("不透明度")}</span>
          <span class="mono val">{Math.round(profile.opacity * 100)}%</span>
        </div>
        <input
          type="range"
          class="range"
          min="15"
          max="100"
          step="1"
          value={Math.round(profile.opacity * 100)}
          aria-label={t("不透明度")}
          oninput={(e) => {
            profile.opacity = clampOpacity(Number(e.currentTarget.value) / 100);
          }}
          onchange={() => saveKeyLayouts()}
        />
        <div class="mrow" style="margin-top: 6px">
          <span>{t("自动半隐")}</span>
          <button
            type="button"
            class="mini-sw"
            class:on={profile.autoDim}
            role="switch"
            aria-checked={profile.autoDim}
            aria-label={t("自动半隐")}
            onclick={() => {
              profile.autoDim = !profile.autoDim;
              saveKeyLayouts();
            }}
          ></button>
        </div>
      </div>
    {/if}

    {#if selectedItem && (!drag || !drag.moved)}
      <div
        class="popover inspector"
        style={inspectorStyle}
        onpointerdown={(e) => e.stopPropagation()}
      >
        {#if selectedItem.type === "key"}
          <div class="insp-head">
            <div class="tabs" role="tablist">
              {#each [["common", "常用"], ["alpha", "字母"], ["symbol", "符号"], ["fn", "功能"], ["numpad", "小键"], ["system", "系统"]] as const as [cat, label] (cat)}
                <button
                  type="button"
                  role="tab"
                  aria-selected={activeTab === cat}
                  class:on={activeTab === cat}
                  onclick={() => pickTab(cat)}
                >
                  {t(label)}
                </button>
              {/each}
            </div>
            <button type="button" class="del-btn" aria-label={t("删除")} onclick={removeSelected}>
              <Icon name="trash" size={14} />
            </button>
          </div>

          <div
            bind:this={kgridEl}
            class="kgrid"
            class:alpha={activeTab === "alpha"}
            style="max-height: {gridMaxHeight}px"
          >
            {#each KEY_CATALOG[activeTab] as b (b.kind + ":" + b.label)}
              <button
                type="button"
                class="kchip"
                class:on={bindingEquals(selectedItem.binding, b)}
                {...tap(() => {
                  selectedItem.binding = { ...b };
                  if (!bindingCanLatch(b)) selectedItem.latch = false;
                  saveKeyLayouts();
                  haptic();
                })}
              >
                {bindingLabel(b)}
              </button>
            {/each}
          </div>

          <div class="psep"></div>

          <div class="size-row">
            <span class="lbl">{t("大小")}</span>
            <input
              type="range"
              class="range"
              min="65"
              max="160"
              step="1"
              value={Math.round(selectedItem.scale * 100)}
              aria-label={t("大小")}
              oninput={(e) => {
                selectedItem.scale = clampScale(Number(e.currentTarget.value) / 100, true);
              }}
              onchange={() => saveKeyLayouts()}
            />
            <button
              type="button"
              class="pct mono"
              onclick={() => {
                selectedItem.scale = 1;
                saveKeyLayouts();
              }}
            >
              {Math.round(selectedItem.scale * 100)}%
            </button>
          </div>

          <div class="foot-row">
            {#if bindingCanLatch(selectedItem.binding)}
              <div class="mseg">
                <button
                  type="button"
                  class:on={!selectedItem.latch}
                  onclick={() => {
                    selectedItem.latch = false;
                    saveKeyLayouts();
                  }}
                >
                  {t("按住")}
                </button>
                <button
                  type="button"
                  class:on={selectedItem.latch}
                  onclick={() => {
                    selectedItem.latch = true;
                    saveKeyLayouts();
                  }}
                >
                  {t("切换")}
                </button>
              </div>
            {:else}
              <span></span>
            {/if}
            <button
              type="button"
              class="sync-btn"
              onclick={() => applyScaleToPeers(selectedItem)}
            >
              {t("同步同类")}
            </button>
          </div>
        {:else}
          <div class="insp-head">
            <div class="dpad-segs">
              <div class="mseg">
                <button
                  type="button"
                  class:on={selectedItem.bind === "arrows"}
                  onclick={() => {
                    selectedItem.bind = "arrows";
                    saveKeyLayouts();
                  }}
                >
                  ↑↓←→
                </button>
                <button
                  type="button"
                  class:on={selectedItem.bind === "wasd"}
                  onclick={() => {
                    selectedItem.bind = "wasd";
                    saveKeyLayouts();
                  }}
                >
                  WASD
                </button>
                <button
                  type="button"
                  class:on={selectedItem.bind === "numpad"}
                  onclick={() => {
                    selectedItem.bind = "numpad";
                    saveKeyLayouts();
                  }}
                >
                  Num
                </button>
              </div>
              <div class="mseg">
                <button
                  type="button"
                  class:on={selectedItem.eightWay}
                  onclick={() => {
                    selectedItem.eightWay = true;
                    saveKeyLayouts();
                  }}
                >
                  {t("8向")}
                </button>
                <button
                  type="button"
                  class:on={!selectedItem.eightWay}
                  onclick={() => {
                    selectedItem.eightWay = false;
                    saveKeyLayouts();
                  }}
                >
                  {t("4向")}
                </button>
              </div>
            </div>
            <button type="button" class="del-btn" aria-label={t("删除")} onclick={removeSelected}>
              <Icon name="trash" size={14} />
            </button>
          </div>

          <div class="psep"></div>

          <div class="size-row">
            <span class="lbl">{t("大小")}</span>
            <input
              type="range"
              class="range"
              min="65"
              max="160"
              step="1"
              value={Math.round(selectedItem.scale * 100)}
              aria-label={t("大小")}
              oninput={(e) => {
                selectedItem.scale = clampScale(Number(e.currentTarget.value) / 100, true);
              }}
              onchange={() => saveKeyLayouts()}
            />
            <button
              type="button"
              class="pct mono"
              onclick={() => {
                selectedItem.scale = 1;
                saveKeyLayouts();
              }}
            >
              {Math.round(selectedItem.scale * 100)}%
            </button>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{:else}
  <div class="play-layer" class:dimmed={idleDim} style="opacity: {layerOpacity}">
    {#each visibleItems as item (item.id)}
      {@const box = itemCenter(item, bounds)}
      {#if item.type === "key"}
        {@const active = (heldCounts[item.id] ?? 0) > 0 || Boolean(latchedIds[item.id])}
        <button
          type="button"
          class="vkey"
          class:round={item.binding.kind === "sys"}
          class:active
          class:latched={Boolean(latchedIds[item.id])}
          style="left: {box.cx - box.width / 2}px; top: {box.cy - box.height / 2}px; width: {box.width}px; height: {box.height}px"
          aria-label={bindingLabel(item.binding)}
          {@attach region}
          {...playKeyHandlers(item)}
        >
          {@render keyFace(item)}
          {#if effectiveLatch(item)}<span class="latch-dot" aria-hidden="true"></span>{/if}
        </button>
      {:else}
        {@const st = dpadStates[item.id] ?? IDLE_DPAD}
        {@const knob = dpadKnobs[item.id] ?? IDLE_KNOB}
        <div
          role="group"
          class="dpad"
          style="left: {box.cx - box.width / 2}px; top: {box.cy - box.height / 2}px; width: {box.width}px; height: {box.height}px"
          aria-label={t("方向盘")}
          {@attach region}
          {...playDpadHandlers(item)}
        >
          {@render dpadFace(item, st, knob)}
        </div>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .play-layer,
  .items-layer {
    position: absolute;
    inset: 0;
    pointer-events: none;
    transition: opacity 0.28s ease;
  }

  .edit-layer {
    position: absolute;
    inset: 0;
    background-color: rgba(8, 9, 12, 0.34);
    background-image: radial-gradient(rgba(255, 255, 255, 0.14) 1px, transparent 1px);
    background-size: 20px 20px;
    user-select: none;
    -webkit-user-select: none;
  }

  .safe-box {
    position: absolute;
    box-sizing: border-box;
    border: 1px dashed rgba(255, 255, 255, 0.16);
    border-radius: 14px;
    pointer-events: none;
  }

  .guide-v,
  .guide-h {
    position: absolute;
    background: rgba(130, 175, 255, 0.55);
    pointer-events: none;
  }
  .guide-v {
    top: 0;
    bottom: 0;
    width: 1px;
  }
  .guide-h {
    left: 0;
    right: 0;
    height: 1px;
  }

  .vkey {
    all: unset;
    position: absolute;
    box-sizing: border-box;
    display: grid;
    place-items: center;
    border-radius: 13px;
    background: rgba(20, 21, 25, 0.56);
    backdrop-filter: blur(12px) saturate(1.15);
    -webkit-backdrop-filter: blur(12px) saturate(1.15);
    border: 1px solid rgba(255, 255, 255, 0.16);
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.24);
    color: rgba(246, 246, 248, 0.94);
    font-weight: 500;
    letter-spacing: 0.01em;
    white-space: nowrap;
    pointer-events: auto;
    cursor: pointer;
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    -webkit-tap-highlight-color: transparent;
    transition: background-color 0.1s ease, border-color 0.1s ease, transform 0.08s ease;
  }
  .vkey.round {
    border-radius: 999px;
  }
  .vkey.active {
    background: rgba(94, 140, 244, 0.34);
    border-color: rgba(146, 182, 255, 0.78);
    color: #fff;
    transform: scale(0.95);
  }
  .vkey.latched {
    background: rgba(94, 140, 244, 0.4);
    border-color: rgba(154, 188, 255, 0.88);
    transform: none;
  }

  .latch-dot {
    position: absolute;
    top: 5px;
    right: 5px;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.42);
  }
  .vkey.latched .latch-dot {
    background: #9ec0ff;
    box-shadow: 0 0 6px rgba(158, 192, 255, 0.9);
  }

  .dpad {
    position: absolute;
    box-sizing: border-box;
    border-radius: 50%;
    background: rgba(20, 21, 25, 0.5);
    backdrop-filter: blur(12px) saturate(1.15);
    -webkit-backdrop-filter: blur(12px) saturate(1.15);
    border: 1px solid rgba(255, 255, 255, 0.15);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.28);
    pointer-events: auto;
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    -webkit-tap-highlight-color: transparent;
  }

  .dpad svg {
    display: block;
    pointer-events: none;
  }

  .dp-ring {
    fill: none;
    stroke: rgba(255, 255, 255, 0.06);
    stroke-width: 1;
  }
  .dp-puck {
    position: absolute;
    left: 50%;
    top: 50%;
    width: 28%;
    height: 28%;
    box-sizing: border-box;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.2);
    pointer-events: none;
    will-change: transform;
    transition:
      transform 0.14s cubic-bezier(0.22, 1, 0.36, 1),
      background-color 0.1s ease,
      border-color 0.1s ease;
  }
  .dp-puck.active {
    background: rgba(94, 140, 244, 0.34);
    border-color: rgba(146, 182, 255, 0.82);
    transition:
      background-color 0.08s ease,
      border-color 0.08s ease;
  }
  .dp-diag {
    stroke: rgba(255, 255, 255, 0.16);
    stroke-width: 1.4;
    stroke-linecap: round;
  }
  .dp-diag.on {
    stroke: #9ec0ff;
    stroke-width: 2.2;
  }
  .dp-dir path {
    fill: none;
    stroke: rgba(244, 244, 246, 0.72);
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .dp-dir text {
    fill: rgba(244, 244, 246, 0.76);
    font-size: 13px;
    font-weight: 600;
  }
  .dp-dir.on path {
    stroke: #9ec0ff;
    stroke-width: 2.6;
  }
  .dp-dir.on text {
    fill: #b8d2ff;
  }

  .edit-item {
    cursor: grab;
  }
  .edit-item.selected {
    border-color: #8fb4ff;
    box-shadow: 0 0 0 2px rgba(130, 172, 255, 0.42), 0 8px 24px rgba(0, 0, 0, 0.38);
  }

  .toolbar {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    height: 42px;
    padding: 0 5px;
    border-radius: 999px;
    background: rgba(22, 23, 27, 0.88);
    backdrop-filter: blur(18px) saturate(1.3);
    -webkit-backdrop-filter: blur(18px) saturate(1.3);
    border: 1px solid rgba(255, 255, 255, 0.14);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    gap: 2px;
    max-width: calc(100% - 20px);
  }
  .tbtn {
    all: unset;
    box-sizing: border-box;
    height: 32px;
    padding: 0 11px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 12.5px;
    color: rgba(240, 240, 242, 0.88);
    white-space: nowrap;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }
  .tbtn:hover,
  .tbtn.on {
    background: rgba(255, 255, 255, 0.11);
    color: #fff;
  }
  .tsep {
    width: 1px;
    height: 16px;
    background: rgba(255, 255, 255, 0.12);
    margin: 0 2px;
  }
  .tdone {
    all: unset;
    box-sizing: border-box;
    height: 32px;
    padding: 0 14px;
    border-radius: 999px;
    background: #ececea;
    color: #151517;
    font-size: 12.5px;
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }

  .popover {
    position: absolute;
    box-sizing: border-box;
    padding: 10px;
    border-radius: 14px;
    background: rgba(24, 25, 29, 0.94);
    backdrop-filter: blur(20px) saturate(1.3);
    -webkit-backdrop-filter: blur(20px) saturate(1.3);
    border: 1px solid rgba(255, 255, 255, 0.13);
    box-shadow: 0 20px 48px rgba(0, 0, 0, 0.55);
    color: #ececea;
    font-size: 12.5px;
  }
  .top-pop {
    left: 50%;
    transform: translateX(-50%);
    width: 216px;
  }
  .opacity-pop {
    width: 204px;
  }

  .plist {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 150px;
    overflow-y: auto;
    touch-action: pan-y;
    overscroll-behavior: contain;
    scrollbar-width: none;
  }
  .pitem {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .pname {
    all: unset;
    box-sizing: border-box;
    flex: 1;
    min-width: 0;
    height: 32px;
    padding: 0 8px;
    border-radius: 8px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    color: rgba(236, 236, 238, 0.85);
    cursor: pointer;
  }
  .pname span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pname:hover,
  .pitem.active .pname {
    background: rgba(255, 255, 255, 0.08);
    color: #fff;
  }
  .pdel,
  .del-btn {
    all: unset;
    box-sizing: border-box;
    width: 32px;
    height: 32px;
    border-radius: 8px;
    display: grid;
    place-items: center;
    color: rgba(255, 145, 135, 0.85);
    cursor: pointer;
    flex: 0 0 auto;
    -webkit-tap-highlight-color: transparent;
  }
  .pdel:hover,
  .del-btn:hover {
    background: rgba(255, 120, 110, 0.14);
    color: #ff9c92;
  }

  .psep {
    height: 1px;
    background: rgba(255, 255, 255, 0.09);
    margin: 8px 0;
  }

  .fork-btn {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    height: 30px;
    padding: 0 8px;
    border-radius: 8px;
    display: flex;
    align-items: center;
    gap: 6px;
    color: rgba(236, 236, 238, 0.8);
    cursor: pointer;
  }
  .fork-btn:hover {
    background: rgba(255, 255, 255, 0.08);
    color: #fff;
  }
  .fork-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .fork-input {
    all: unset;
    box-sizing: border-box;
    flex: 1;
    min-width: 0;
    height: 30px;
    padding: 0 8px;
    border-radius: 7px;
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.14);
    color: #fff;
    font-size: 12px;
  }
  .pill-action {
    all: unset;
    box-sizing: border-box;
    height: 30px;
    padding: 0 10px;
    border-radius: 7px;
    background: #ececea;
    color: #151517;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
  }

  .mrow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    min-height: 26px;
  }
  .val {
    color: rgba(236, 236, 238, 0.68);
    font-size: 12px;
  }

  .range {
    width: 100%;
    margin: 4px 0 0;
    accent-color: #96b8ff;
    height: 28px;
    cursor: pointer;
    touch-action: pan-x;
  }

  .mini-sw {
    all: unset;
    box-sizing: border-box;
    position: relative;
    width: 34px;
    height: 20px;
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.2);
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .mini-sw::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 8px;
    background: #ececea;
    transition: transform 0.15s ease;
  }
  .mini-sw.on {
    background: var(--accent, #4a78e0);
  }
  .mini-sw.on::after {
    transform: translateX(14px);
    background: #fff;
  }

  .insp-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin-bottom: 8px;
  }
  .tabs,
  .mseg {
    display: inline-flex;
    padding: 2px;
    border-radius: 9px;
    background: rgba(255, 255, 255, 0.07);
    gap: 2px;
    max-width: 100%;
    min-width: 0;
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
    touch-action: pan-x;
    scrollbar-width: none;
  }
  .tabs::-webkit-scrollbar {
    display: none;
  }
  .tabs button {
    all: unset;
    box-sizing: border-box;
    height: 32px;
    padding: 0 9.5px;
    border-radius: 7px;
    font-size: 12px;
    color: rgba(236, 236, 238, 0.72);
    cursor: pointer;
    white-space: nowrap;
    -webkit-tap-highlight-color: transparent;
  }
  .mseg button {
    all: unset;
    box-sizing: border-box;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 12px;
    color: rgba(236, 236, 238, 0.72);
    cursor: pointer;
    white-space: nowrap;
    -webkit-tap-highlight-color: transparent;
  }
  .tabs button.on,
  .mseg button.on {
    background: rgba(255, 255, 255, 0.16);
    color: #fff;
    font-weight: 500;
  }
  .insp-head .del-btn {
    margin-right: 2px;
  }
  .dpad-segs {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
    min-width: 0;
  }

  .kgrid {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 6px;
    padding-right: 2px;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    touch-action: pan-y;
    overscroll-behavior: contain;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.22) transparent;
  }
  .kgrid::-webkit-scrollbar {
    width: 3px;
  }
  .kgrid::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.22);
    border-radius: 3px;
  }
  .kgrid.alpha {
    grid-template-columns: repeat(6, minmax(0, 1fr));
  }
  .kchip {
    all: unset;
    box-sizing: border-box;
    height: 36px;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.07);
    color: rgba(242, 242, 245, 0.9);
    font-size: 12.5px;
    display: grid;
    place-items: center;
    cursor: pointer;
    white-space: nowrap;
    touch-action: pan-y;
    -webkit-tap-highlight-color: transparent;
    transition: background-color 0.08s ease, transform 0.08s ease;
  }
  .kchip:hover {
    background: rgba(255, 255, 255, 0.12);
  }
  .kchip:active {
    background: rgba(255, 255, 255, 0.18);
    transform: scale(0.96);
  }
  .kchip.on {
    background: rgba(98, 144, 245, 0.36);
    box-shadow: inset 0 0 0 1px rgba(148, 184, 255, 0.85);
    color: #fff;
    font-weight: 500;
  }

  .size-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .size-row .lbl {
    flex: 0 0 auto;
    color: rgba(236, 236, 238, 0.75);
    font-size: 12px;
  }
  .size-row .range {
    flex: 1;
    margin: 0;
  }
  .pct {
    all: unset;
    box-sizing: border-box;
    flex: 0 0 auto;
    min-width: 44px;
    height: 28px;
    padding: 0 7px;
    border-radius: 7px;
    background: rgba(255, 255, 255, 0.08);
    color: rgba(236, 236, 238, 0.9);
    font-size: 11.5px;
    text-align: center;
    line-height: 28px;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }
  .pct:hover {
    background: rgba(255, 255, 255, 0.14);
  }

  .foot-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 8px;
  }
  .sync-btn {
    all: unset;
    box-sizing: border-box;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    background: rgba(255, 255, 255, 0.08);
    color: rgba(236, 236, 238, 0.85);
    font-size: 12px;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }
  .sync-btn:hover {
    background: rgba(255, 255, 255, 0.14);
    color: #fff;
  }

  .fade {
    animation: fade 0.16s ease;
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
    .play-layer,
    .items-layer,
    .vkey,
    .dp-puck,
    .mini-sw,
    .mini-sw::after {
      transition: none;
    }
  }
</style>
