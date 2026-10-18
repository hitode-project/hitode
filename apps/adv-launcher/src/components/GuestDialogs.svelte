<script lang="ts" module>
  /** A guest window a dialog event names, echoed back as received. */
  export type DialogTarget = { dialog: number; hwnd: number; parent: number; id: number; createdUs: number };
  export type DialogControl = {
    target: DialogTarget;
    kind: "push" | "check" | "radio" | "group" | "frame" | "label" | "edit" | "combo" | "list" | "track" | "unavailable";
    text: string;
    x: number;
    y: number;
    width: number;
    height: number;
    enabled: boolean;
    check: number;
    items: string[];
    selection: number;
    range: [number, number] | null;
    value: number;
  };
  /** A visible guest dialog, in its client pixels (mobile_view.rs `game_dialogs`). */
  export type GuestDialog = { target: DialogTarget; text: string; width: number; height: number; controls: DialogControl[] };
</script>

<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { call } from "../lib/bridge";
  import { modal } from "../lib/overlay";

  /**
   * The game's own dialogs (MessageBox and resource dialogs) on mobile, which
   * has no native projection. The guest owns every state: a click only
   * reports the press, and the next `game_dialogs` shows the result.
   */
  let { dialogs }: { dialogs: GuestDialog[] } = $props();

  let width = $state(window.innerWidth);
  let height = $state(window.innerHeight);

  /** Guest pixels are sized for a mouse; enlarge them for fingers when they fit. */
  function scale(dialog: GuestDialog) {
    const fit = Math.min((width - 32) / Math.max(1, dialog.width), (height - 96) / Math.max(1, dialog.height));
    return Math.max(0.5, Math.min(1.6, fit));
  }

  type Input =
    | { kind: "click"; target: DialogTarget }
    | { kind: "select"; target: DialogTarget; index: number }
    | { kind: "track"; target: DialogTarget; value: number }
    | { kind: "text"; target: DialogTarget; text: string };
  function send(input: Input) {
    void call("game_dialog", { input }, null).catch((error) => console.error("game_dialog failed", error));
  }

  const box = (c: DialogControl, s: number) =>
    `left: ${c.x * s}px; top: ${c.y * s}px; width: ${c.width * s}px; height: ${c.height * s}px`;
  /** Group boxes sit under the controls they frame. */
  const framed = (d: GuestDialog) => [
    ...d.controls.filter((c) => c.kind === "group" || c.kind === "frame"),
    ...d.controls.filter((c) => c.kind !== "group" && c.kind !== "frame"),
  ];
</script>

<svelte:window bind:innerWidth={width} bind:innerHeight={height} />

<div class="scrim fade" {@attach modal}>
  {#each dialogs as dialog (dialog.target.hwnd)}
    {@const s = scale(dialog)}
    <div class="dialog" role="dialog" aria-modal="true" aria-label={dialog.text} lang="ja" style="--s: {s}">
      {#if dialog.text}<div class="caption">{dialog.text}</div>{/if}
      <div class="client" style="width: {dialog.width * s}px; height: {dialog.height * s}px">
        {#each framed(dialog) as c (c.target.hwnd)}
          {#if c.kind === "push"}
            <button class="push" style={box(c, s)} disabled={!c.enabled} onclick={() => send({ kind: "click", target: c.target })}>{c.text}</button>
          {:else if c.kind === "check" || c.kind === "radio"}
            <button class="toggle" class:radio={c.kind === "radio"} style={box(c, s)} disabled={!c.enabled}
              role={c.kind === "radio" ? "radio" : "checkbox"} aria-checked={c.check === 1}
              onclick={() => send({ kind: "click", target: c.target })}>
              <span class="mark" class:on={c.check === 1} class:mixed={c.check === 2}></span>
              <span>{c.text}</span>
            </button>
          {:else if c.kind === "group"}
            <div class="group" style={box(c, s)}>{#if c.text}<span>{c.text}</span>{/if}</div>
          {:else if c.kind === "frame"}
            <div class="frame" style={box(c, s)}></div>
          {:else if c.kind === "label"}
            <div class="label" class:off={!c.enabled} style={box(c, s)}>{c.text}</div>
          {:else if c.kind === "edit"}
            <input class="edit" style={box(c, s)} disabled={!c.enabled} value={c.text} autocomplete="off" spellcheck="false"
              onchange={(e) => send({ kind: "text", target: c.target, text: e.currentTarget.value })} />
          {:else if c.kind === "combo"}
            <!-- A combo box's height includes its closed list. -->
            <select class="edit" style="{box(c, s)}; height: {Math.min(c.height, 24) * s}px" disabled={!c.enabled} value={c.selection}
              onchange={(e) => send({ kind: "select", target: c.target, index: Number(e.currentTarget.value) })}>
              {#each c.items as item, i (i)}<option value={i}>{item}</option>{/each}
            </select>
          {:else if c.kind === "list"}
            <div class="list" style={box(c, s)} role="listbox">
              {#each c.items as item, i (i)}
                <button role="option" aria-selected={i === c.selection} class:on={i === c.selection} disabled={!c.enabled}
                  onclick={() => send({ kind: "select", target: c.target, index: i })}>{item}</button>
              {/each}
            </div>
          {:else if c.kind === "track"}
            <input class="track" type="range" style={box(c, s)} disabled={!c.enabled}
              min={c.range?.[0] ?? 0} max={c.range?.[1] ?? 100} value={c.value}
              onchange={(e) => send({ kind: "track", target: c.target, value: Number(e.currentTarget.value) })} />
          {:else}
            <div class="label off" style={box(c, s)}>{c.text || t("不支持的控件")}</div>
          {/if}
        {/each}
      </div>
    </div>
  {/each}
</div>

<style>
  .scrim {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.55);
    z-index: 20;
  }
  .dialog {
    grid-area: 1 / 1;
    border-radius: 14px;
    background: var(--panel);
    border: 1px solid var(--line);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5);
    overflow: hidden;
    font-size: calc(12px * var(--s));
  }
  .caption {
    padding: 12px 16px 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--fg2);
  }
  .client {
    position: relative;
    margin: 8px 12px 12px;
  }
  .client > * {
    position: absolute;
    box-sizing: border-box;
    font: inherit;
    color: var(--fg);
  }
  .push {
    all: unset;
    position: absolute;
    box-sizing: border-box;
    display: grid;
    place-items: center;
    border-radius: 8px;
    background: var(--field);
    cursor: pointer;
  }
  .push:active {
    background: var(--accent);
  }
  .toggle {
    all: unset;
    position: absolute;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 0.5em;
    cursor: pointer;
  }
  .mark {
    flex: 0 0 auto;
    width: 1.1em;
    height: 1.1em;
    border-radius: 4px;
    border: 1.5px solid var(--fg3);
    box-sizing: border-box;
  }
  .radio .mark {
    border-radius: 50%;
  }
  .mark.on {
    background: var(--accent);
    border-color: var(--accent);
    box-shadow: inset 0 0 0 2px var(--panel);
  }
  .mark.mixed {
    background: var(--fg3);
  }
  .push:disabled,
  .toggle:disabled,
  .off {
    opacity: 0.45;
  }
  .group,
  .frame {
    border: 1px solid var(--line);
    border-radius: 6px;
    pointer-events: none;
  }
  .group span {
    position: absolute;
    top: -0.7em;
    left: 0.6em;
    padding: 0 0.3em;
    background: var(--panel);
    color: var(--fg2);
  }
  .label {
    white-space: pre-wrap;
    line-height: 1.35;
    overflow: hidden;
  }
  .edit {
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--field);
    padding: 0 0.4em;
  }
  .list {
    overflow-y: auto;
    border: 1px solid var(--line);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
  }
  .list button {
    all: unset;
    padding: 0.35em 0.5em;
  }
  .list button.on {
    background: var(--accent);
    color: var(--on-accent);
  }
  .track {
    accent-color: var(--accent);
    margin: 0;
  }
</style>
