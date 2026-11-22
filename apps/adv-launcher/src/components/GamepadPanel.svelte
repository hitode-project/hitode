<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import { device } from "../lib/device.svelte";
  import { nav } from "../lib/nav.svelte";
  import { bindingCanLatch, type DpadBind, type KeyBinding } from "../lib/key-layouts";
  import {
    BUTTON_GROUPS,
    effectiveButtonSlot,
    glyphsForDevice,
    isBuiltinModified,
    resolveEffectiveProfileId,
    type GamepadButtonEntry,
    type GamepadButtonId,
    type GlyphOverride,
    type StickRole,
  } from "../lib/gamepad";
  import { gamepad } from "../lib/gamepad.svelte";
  import GamepadKeyPicker from "./GamepadKeyPicker.svelte";
  import SettingsSwitch from "./SettingsSwitch.svelte";
  import Icon from "./Icon.svelte";

  let {
    gameId = null,
    inPause = false,
    compact = false,
    onpickingchange,
  }: {
    gameId?: string | null;
    inPause?: boolean;
    compact?: boolean;
    onpickingchange?: (picking: boolean) => void;
  } = $props();

  let selectedDeviceId = $state<string | null>(null);
  let scope = $state<"game" | "global">("game");
  let subTab = $state<"buttons" | "sticks">("buttons");
  let pickingButton = $state<GamepadButtonId | null>(null);
  let flashedButton = $state<GamepadButtonId | null>(null);
  let savingAs = $state(false);
  let newProfileName = $state("");
  let flashTimer: ReturnType<typeof setTimeout> | undefined;

  /** In pause menu show only connected devices; in Settings show all known devices. */
  const visibleDevices = $derived.by(() => {
    const all = gamepad.config.devices;
    const connected = all.filter((d) => gamepad.isConnected(d.id));
    if (inPause) return connected;
    const disconnected = all.filter((d) => !gamepad.isConnected(d.id));
    return [...connected, ...disconnected];
  });

  const activeDevice = $derived.by(() => {
    if (!visibleDevices.length) return null;
    return visibleDevices.find((d) => d.id === selectedDeviceId) ?? visibleDevices[0]!;
  });

  const activeDeviceId = $derived(activeDevice?.id ?? null);
  const effectiveGameId = $derived(inPause && scope === "game" ? gameId : null);
  const activeProfileId = $derived(
    resolveEffectiveProfileId(gamepad.config, activeDeviceId, effectiveGameId),
  );
  const activeProfile = $derived(gamepad.profileById(activeProfileId));
  const glyphs = $derived(glyphsForDevice(activeDevice));
  const swap = $derived(activeDevice?.swapConfirmCancel ?? false);
  const modified = $derived(isBuiltinModified(activeProfile));
  const hasGameOverride = $derived(Boolean(gameId && gamepad.config.gameProfiles[gameId]));
  const hasDevicePin = $derived(
    !inPause && visibleDevices.length > 1 && Boolean(activeDevice?.defaultProfileId),
  );
  const showCursorSpeed = $derived(
    activeProfile.leftStick === "cursor" || activeProfile.rightStick === "cursor",
  );
  const showStickDpadBind = $derived(
    activeProfile.leftStick === "dpad" || activeProfile.rightStick === "dpad",
  );
  const useInlinePicker = $derived(compact || device.phone);

  // Sync picking state with parent (for mobile back navigation).
  $effect(() => {
    onpickingchange?.(pickingButton !== null);
  });

  // Back button dismisses inline key picker before leaving the panel.
  $effect(() => {
    if (!pickingButton || !useInlinePicker) return;
    return nav.onBack(() => {
      pickingButton = null;
      return true;
    });
  });

  // Press-to-locate: pressing a physical button auto-focuses its device & highlights the row.
  let seenPressSeq = 0;
  $effect(() => {
    const press = gamepad.lastPress;
    if (!press || press.seq === seenPressSeq) return;
    seenPressSeq = press.seq;
    if (visibleDevices.some((d) => d.id === press.deviceId)) {
      selectedDeviceId = press.deviceId;
    }
    if (pickingButton) return;
    if (compact && subTab !== "buttons") subTab = "buttons";
    flashedButton = press.button;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => {
      flashedButton = null;
    }, 900);
    queueMicrotask(() => {
      const el = document.querySelector(`[data-gp-row="${press.button}"]`);
      el?.scrollIntoView({ behavior: "smooth", block: "nearest" });
    });
  });

  function formatBinding(entry: GamepadButtonEntry | undefined): string | null {
    if (!entry) return null;
    const b = entry.binding;
    if (b.kind === "mouse") {
      if (b.button === "right") return t("鼠标右键");
      if (b.button === "middle") return t("鼠标中键");
      return t("鼠标左键");
    }
    if (b.kind === "sys") {
      if (b.sys === "menu") return t("暂停菜单");
      if (b.sys === "hide") return t("显隐控件");
      return t("文字输入");
    }
    if (b.kind === "wheel" || b.label === "退格") return t(b.label);
    return b.label;
  }

  function onSelectProfile(profileId: string) {
    if (inPause && scope === "game" && gameId) {
      const fallback = activeDevice?.defaultProfileId ?? gamepad.config.globalProfileId;
      gamepad.setGameProfile(gameId, profileId === fallback ? null : profileId);
    } else if (!inPause && visibleDevices.length > 1 && activeDevice) {
      gamepad.setDeviceProfile(
        activeDevice.id,
        profileId === gamepad.config.globalProfileId ? null : profileId,
      );
    } else {
      gamepad.setGlobalProfile(profileId);
    }
  }

  function openSaveAs() {
    newProfileName = `${t(activeProfile.name)} ${t("副本")}`;
    savingAs = true;
  }

  function commitSaveAs() {
    const id = gamepad.saveAsProfile(activeProfile.id, newProfileName);
    if (id) onSelectProfile(id);
    savingAs = false;
  }

  function handlePickKey(binding: KeyBinding) {
    if (!pickingButton) return;
    const slot = effectiveButtonSlot(pickingButton, swap);
    gamepad.setButtonBinding(activeProfile.id, slot, binding);
  }

  function handleLatch(latch: boolean) {
    if (!pickingButton) return;
    const slot = effectiveButtonSlot(pickingButton, swap);
    gamepad.setButtonLatch(activeProfile.id, slot, latch);
  }

  function handleClear() {
    if (!pickingButton) return;
    const slot = effectiveButtonSlot(pickingButton, swap);
    gamepad.setButtonBinding(activeProfile.id, slot, null);
  }

  const stickRoles: { id: StickRole; label: string }[] = [
    { id: "cursor", label: "光标" },
    { id: "dpad", label: "方向" },
    { id: "wheel", label: "滚轮" },
    { id: "off", label: "关闭" },
  ];

  const dpadOptions: { id: DpadBind; label: string }[] = [
    { id: "arrows", label: "↑↓←→" },
    { id: "wasd", label: "WASD" },
    { id: "numpad", label: "小键" },
  ];

  const glyphOptions: { id: GlyphOverride; label: string }[] = [
    { id: "auto", label: "自动" },
    { id: "xbox", label: "Xbox" },
    { id: "ps", label: "PS" },
    { id: "switch", label: "Switch" },
  ];
</script>

{#snippet sticksSection()}
  <div class="card">
    {#if activeDevice}
      <div class="line-row">
        <span class="label-group">
          <span>{t("互换确认与取消")}</span>
          <span class="mini-tag mono">
            {swap
              ? `${glyphs.east.glyph} / ${glyphs.south.glyph}`
              : `${glyphs.south.glyph} / ${glyphs.east.glyph}`}
          </span>
        </span>
        <SettingsSwitch
          touch={device.touch}
          label={t("互换确认与取消")}
          checked={activeDevice.swapConfirmCancel}
          onchange={(v) => gamepad.setDeviceSwap(activeDevice.id, v)}
        />
      </div>
      <div class="line-row wrap">
        <span>{t("键标显示")}</span>
        <div class="seg" role="radiogroup" aria-label={t("键标显示")}>
          {#each glyphOptions as opt (opt.id)}
            <button
              type="button"
              class:on={activeDevice.glyphOverride === opt.id}
              onclick={() => gamepad.setDeviceGlyph(activeDevice.id, opt.id)}
            >
              {opt.id === "auto" ? t(opt.label) : opt.label}
            </button>
          {/each}
        </div>
      </div>
    {/if}
    {#if device.touch}
      <div class="line-row">
        <span>{t("连接手柄时隐藏屏幕控件")}</span>
        <SettingsSwitch
          touch
          label={t("连接手柄时隐藏屏幕控件")}
          checked={gamepad.config.autoHideTouch}
          onchange={(v) => gamepad.setAutoHideTouch(v)}
        />
      </div>
    {/if}
  </div>

  <div class="group-block">
    <div class="group-head">{t("摇杆")}</div>
    <div class="card">
      <div class="line-row wrap">
        <span>{t("左摇杆")}</span>
        <div class="seg" role="radiogroup" aria-label={t("左摇杆")}>
          {#each stickRoles as r (r.id)}
            <button
              type="button"
              class:on={activeProfile.leftStick === r.id}
              onclick={() => gamepad.setStickRole(activeProfile.id, "leftStick", r.id)}
            >
              {t(r.label)}
            </button>
          {/each}
        </div>
      </div>
      <div class="line-row wrap">
        <span>{t("右摇杆")}</span>
        <div class="seg" role="radiogroup" aria-label={t("右摇杆")}>
          {#each stickRoles as r (r.id)}
            <button
              type="button"
              class:on={activeProfile.rightStick === r.id}
              onclick={() => gamepad.setStickRole(activeProfile.id, "rightStick", r.id)}
            >
              {t(r.label)}
            </button>
          {/each}
        </div>
      </div>
      {#if showCursorSpeed}
        <div class="line-row">
          <span>{t("光标速度")}</span>
          <div class="speed-box">
            <input
              type="range"
              min="0.5"
              max="2.0"
              step="0.1"
              value={activeProfile.cursorSpeed}
              aria-label={t("光标速度")}
              oninput={(e) =>
                gamepad.setCursorSpeed(activeProfile.id, parseFloat(e.currentTarget.value))}
            />
            <span class="mono speed-val">{activeProfile.cursorSpeed.toFixed(1)}×</span>
          </div>
        </div>
      {/if}
      {#if showStickDpadBind}
        <div class="line-row wrap">
          <span>{t("摇杆方向键位")}</span>
          <div class="seg" role="radiogroup" aria-label={t("摇杆方向键位")}>
            {#each dpadOptions as opt (opt.id)}
              <button
                type="button"
                class:on={activeProfile.stickDpadBind === opt.id}
                onclick={() => gamepad.setStickDpadBind(activeProfile.id, opt.id)}
              >
                {opt.id === "numpad" ? t(opt.label) : opt.label}
              </button>
            {/each}
          </div>
        </div>
      {/if}
    </div>
  </div>
{/snippet}

{#snippet buttonsSection()}
  <div class="btn-grid" class:single={compact || device.phone}>
    {#each BUTTON_GROUPS as group (group.id)}
      <div class="group-block">
        <div class="group-head">
          <span>{t(group.title)}</span>
          {#if group.hasDpadPresets}
            <div class="dpad-presets">
              {#each dpadOptions as opt (opt.id)}
                <button
                  type="button"
                  class="preset-chip"
                  onclick={() => gamepad.applyDpadPreset(activeProfile.id, opt.id)}
                >
                  {opt.id === "numpad" ? t(opt.label) : opt.label}
                </button>
              {/each}
            </div>
          {/if}
        </div>
        <div class="card">
          {#each group.buttons as btnId (btnId)}
            {@const g = glyphs[btnId]}
            {@const slot = effectiveButtonSlot(btnId, swap)}
            {@const entry = activeProfile.bindings[slot]}
            {@const label = formatBinding(entry)}
            {@const swapped = swap && (btnId === "south" || btnId === "east")}
            <button
              type="button"
              data-gp-row={btnId}
              class="map-row"
              class:flash={flashedButton === btnId}
              onclick={() => (pickingButton = btnId)}
            >
              <span class="map-left">
                <span class="badge" class:round={g.shape === "round"} class:pill={g.shape === "pill"}>
                  {g.glyph}
                </span>
                <span class="btn-name">{t(g.name)}</span>
                {#if swapped}<span class="sub-tag">{t("已互换")}</span>{/if}
              </span>
              <span class="map-right">
                {#if label}
                  <span class="bind-pill">{label}</span>
                {:else}
                  <span class="bind-pill empty">{t("未绑定")}</span>
                {/if}
                {#if entry?.latch && bindingCanLatch(entry.binding)}
                  <span class="latch-pill">{t("切换")}</span>
                {/if}
                <Icon name="chevron-right" size={14} />
              </span>
            </button>
          {/each}
        </div>
      </div>
    {/each}
  </div>
{/snippet}

{#if pickingButton && useInlinePicker}
  {@const slot = effectiveButtonSlot(pickingButton, swap)}
  <GamepadKeyPicker
    inline
    glyph={glyphs[pickingButton]}
    entry={activeProfile.bindings[slot]}
    backLabel={compact ? "返回" : "外接手柄"}
    onpick={handlePickKey}
    onlatch={handleLatch}
    onclear={handleClear}
    onclose={() => (pickingButton = null)}
  />
{:else}
  <div class="gp-panel" class:compact>
    {#if inPause && gameId}
      <div class="scope-bar">
        <div class="seg" role="radiogroup" aria-label={t("作用域")}>
          <button type="button" class:on={scope === "game"} onclick={() => (scope = "game")}>
            {t("仅此游戏")}
          </button>
          <button type="button" class:on={scope === "global"} onclick={() => (scope = "global")}>
            {t("全局默认")}
          </button>
        </div>
        {#if scope === "game" && hasGameOverride}
          <button type="button" class="quiet" onclick={() => gamepad.setGameProfile(gameId, null)}>
            {t("恢复跟随全局")}
          </button>
        {/if}
      </div>
    {/if}

    {#if visibleDevices.length > 1}
      <div class="device-chips scroll" role="tablist" aria-label={t("已识别手柄")}>
        {#each visibleDevices as dev (dev.id)}
          {@const connected = gamepad.isConnected(dev.id)}
          <button
            type="button"
            role="tab"
            aria-selected={activeDevice?.id === dev.id}
            class="dev-chip"
            class:on={activeDevice?.id === dev.id}
            onclick={() => (selectedDeviceId = dev.id)}
          >
            <span class="dot" class:live={connected} aria-hidden="true"></span>
            <span class="dev-name">{dev.name}</span>
          </button>
        {/each}
      </div>
    {/if}

    <!-- Profile & Device Header Card -->
    <div class="card profile-card">
      {#if !compact}
        <div class="line-row wrap">
          <span class="dev-status">
            <span
              class="dot"
              class:live={Boolean(activeDevice && gamepad.isConnected(activeDevice.id))}
              aria-hidden="true"
            ></span>
            <span class="dev-title">{activeDevice?.name ?? t("未连接手柄")}</span>
            <span class="sub-tag">
              {activeDevice && gamepad.isConnected(activeDevice.id) ? t("已连接") : t("未连接")}
            </span>
          </span>
          <div class="profile-actions">
            {#if modified}
              <button
                type="button"
                class="quiet"
                onclick={() => gamepad.resetBuiltinProfile(activeProfile.id)}
              >
                {t("恢复默认")}
              </button>
            {/if}
            <button type="button" class="quiet" onclick={openSaveAs}>{t("另存为…")}</button>
            {#if !activeProfile.builtin}
              <button
                type="button"
                class="quiet danger"
                onclick={() => gamepad.deleteProfile(activeProfile.id)}
              >
                {t("删除")}
              </button>
            {/if}
            {#if !inPause && activeDevice && !gamepad.isConnected(activeDevice.id) && visibleDevices.length > 1}
              <button
                type="button"
                class="quiet danger"
                onclick={() => gamepad.forgetDevice(activeDevice.id)}
              >
                {t("移除记录")}
              </button>
            {/if}
          </div>
        </div>
      {/if}

      <div class="line-row wrap">
        <span class="label-muted">{hasDevicePin ? t("此手柄方案") : t("按键方案")}</span>
        <div class="profile-select-wrap">
          <select
            class="profile-select"
            aria-label={t("按键方案")}
            value={activeProfile.id}
            onchange={(e) => onSelectProfile(e.currentTarget.value)}
          >
            {#each gamepad.config.profiles as p (p.id)}
              <option value={p.id}>
                {p.builtin ? t(p.name) : p.name}{isBuiltinModified(p) ? ` (${t("已修改")})` : ""}
              </option>
            {/each}
          </select>
          {#if compact}
            {#if modified}
              <button
                type="button"
                class="quiet"
                onclick={() => gamepad.resetBuiltinProfile(activeProfile.id)}
              >
                {t("重置")}
              </button>
            {/if}
            <button type="button" class="quiet" onclick={openSaveAs}>+</button>
          {:else if hasDevicePin && activeDevice}
            <button
              type="button"
              class="quiet"
              onclick={() => gamepad.setDeviceProfile(activeDevice.id, null)}
            >
              {t("跟随全局")}
            </button>
          {/if}
        </div>
      </div>

      {#if savingAs}
        <form
          class="save-as-row"
          onsubmit={(e) => {
            e.preventDefault();
            commitSaveAs();
          }}
        >
          <input
            type="text"
            class="save-input"
            bind:value={newProfileName}
            placeholder={t("输入方案名称")}
          />
          <button type="submit" class="primary save-btn">{t("创建")}</button>
          <button type="button" class="quiet" onclick={() => (savingAs = false)}>{t("取消")}</button>
        </form>
      {/if}
    </div>

    {#if compact}
      <!-- Landscape mobile drawer splits buttons vs sticks to keep 390px height effortless -->
      <div class="subtab-bar">
        <div class="seg" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "buttons"}
            class:on={subTab === "buttons"}
            onclick={() => (subTab = "buttons")}
          >
            {t("按键")}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "sticks"}
            class:on={subTab === "sticks"}
            onclick={() => (subTab = "sticks")}
          >
            {t("摇杆与习惯")}
          </button>
        </div>
      </div>
      {#if subTab === "buttons"}
        {@render buttonsSection()}
      {:else}
        {@render sticksSection()}
        {#if !activeProfile.builtin}
          <div class="compact-footer">
            <button
              type="button"
              class="quiet danger"
              onclick={() => gamepad.deleteProfile(activeProfile.id)}
            >
              {t("删除当前方案")}
            </button>
          </div>
        {/if}
      {/if}
    {:else}
      {@render sticksSection()}
      {@render buttonsSection()}
    {/if}
  </div>

  {#if pickingButton && !useInlinePicker}
    {@const slot = effectiveButtonSlot(pickingButton, swap)}
    <GamepadKeyPicker
      glyph={glyphs[pickingButton]}
      entry={activeProfile.bindings[slot]}
      onpick={handlePickKey}
      onlatch={handleLatch}
      onclear={handleClear}
      onclose={() => (pickingButton = null)}
    />
  {/if}
{/if}

<style>
  .gp-panel {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .gp-panel.compact {
    gap: 10px;
  }
  .scope-bar,
  .subtab-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    flex-wrap: wrap;
  }
  .device-chips {
    display: flex;
    align-items: center;
    gap: 6px;
    overflow-x: auto;
    padding-bottom: 2px;
  }
  .dev-chip {
    all: unset;
    box-sizing: border-box;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-radius: 9px;
    background: var(--field);
    border: 1px solid transparent;
    color: var(--fg2);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    white-space: nowrap;
    flex: none;
  }
  .dev-chip.on {
    background: var(--sel);
    border-color: var(--accent);
    color: var(--fg);
  }
  .dev-name {
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--fg3);
    flex: none;
  }
  .dot.live {
    background: #34c759;
  }
  .card {
    background: var(--bg2);
    border: 1px solid var(--line);
    border-radius: 12px;
    overflow: hidden;
  }
  .line-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    min-height: 44px;
    padding: 8px 12px;
    font-size: 13px;
    color: var(--fg);
  }
  .line-row + .line-row {
    border-top: 1px solid var(--line);
  }
  .line-row.wrap {
    flex-wrap: wrap;
  }
  .dev-status {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .dev-title {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .profile-actions,
  .profile-select-wrap {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .label-muted {
    font-size: 12px;
    color: var(--fg2);
  }
  .label-group {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .mini-tag {
    font-size: 11px;
    padding: 2px 6px;
    border-radius: 6px;
    background: var(--field);
    color: var(--fg2);
  }
  .sub-tag {
    font-size: 11px;
    color: var(--fg3);
    white-space: nowrap;
  }
  .profile-select {
    height: 30px;
    padding: 0 8px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--field);
    color: var(--fg);
    font-size: 12px;
    max-width: 200px;
  }
  .save-as-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-top: 1px solid var(--line);
  }
  .save-input {
    flex: 1 1 0;
    min-width: 0;
    height: 32px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--field);
    color: var(--fg);
    font-size: 13px;
  }
  .save-btn {
    height: 32px;
    padding: 0 14px;
    font-size: 12px;
    border-radius: 8px;
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    border-radius: 9px;
    background: var(--field);
    gap: 2px;
    flex: none;
  }
  .seg button {
    all: unset;
    box-sizing: border-box;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg2);
    cursor: pointer;
    white-space: nowrap;
  }
  .seg button.on {
    background: var(--bg2);
    color: var(--fg);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.14);
  }
  .speed-box {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1 1 0;
    max-width: 200px;
    justify-content: flex-end;
  }
  .speed-box input[type="range"] {
    width: 100%;
    accent-color: var(--accent);
  }
  .speed-val {
    font-size: 12px;
    color: var(--fg2);
    width: 34px;
    text-align: right;
  }
  .group-block {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .group-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 4px;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
  }
  .dpad-presets {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .preset-chip {
    all: unset;
    box-sizing: border-box;
    padding: 2px 7px;
    border-radius: 6px;
    background: var(--field);
    color: var(--fg2);
    font-size: 11px;
    cursor: pointer;
  }
  .preset-chip:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .btn-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px;
  }
  .btn-grid.single {
    grid-template-columns: minmax(0, 1fr);
  }
  @media (max-width: 720px) {
    .btn-grid {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .map-row {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    min-height: 42px;
    padding: 8px 12px;
    cursor: pointer;
    transition: background-color 140ms ease;
  }
  .map-row + .map-row {
    border-top: 1px solid var(--line);
  }
  .map-row:hover {
    background: var(--hover);
  }
  .map-row.flash {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .map-left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .btn-name {
    font-size: 13px;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .map-right {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    color: var(--fg3);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 600;
    background: var(--field);
    border: 1px solid var(--line);
    color: var(--fg);
    flex: none;
  }
  .badge.round {
    width: 26px;
    height: 26px;
    border-radius: 50%;
  }
  .badge.pill {
    min-width: 36px;
    height: 24px;
    padding: 0 7px;
    border-radius: 7px;
  }
  .bind-pill {
    padding: 3px 8px;
    border-radius: 6px;
    background: var(--field);
    color: var(--fg);
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
  }
  .bind-pill.empty {
    background: transparent;
    border: 1px dashed var(--line);
    color: var(--fg3);
  }
  .latch-pill {
    padding: 2px 6px;
    border-radius: 5px;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    color: var(--accent);
    font-size: 11px;
    font-weight: 600;
  }
  .danger {
    color: #e5484d;
  }
  .compact-footer {
    display: flex;
    justify-content: flex-end;
    padding-top: 4px;
  }
  :global(:root[data-touch]) .map-row {
    min-height: 46px;
  }
  :global(:root[data-touch]) .seg button {
    height: 34px;
    padding: 0 11px;
    font-size: 13px;
  }
  :global(:root[data-touch]) .profile-select {
    height: 36px;
    font-size: 13px;
  }
</style>
