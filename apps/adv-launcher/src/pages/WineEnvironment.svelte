<script lang="ts">
  import { onMount } from "svelte";
  import Icon from "../components/Icon.svelte";
  import PhoneSettingsHeader from "../components/PhoneSettingsHeader.svelte";
  import { preview } from "../lib/bridge";
  import { device } from "../lib/device.svelte";
  import { revealDrive } from "../lib/host";
  import { t } from "../lib/i18n.svelte";
  import { nav } from "../lib/nav.svelte";
  import { storageSize } from "../lib/storage";
  import { wineEnv, type WineCleanupTarget } from "../lib/wine-env.svelte";

  interface TargetOption {
    id: WineCleanupTarget;
    title: string;
    desc: string;
    badge?: string;
    badgeKind?: "safe" | "danger";
    danger?: boolean;
  }

  const OPTIONS: TargetOption[] = [
    {
      id: "temp",
      title: "临时文件",
      desc: "清理安装器与程序残留的 windows/temp、users/*/Temp 及临时挂载目录",
      badge: "安全",
      badgeKind: "safe",
    },
    {
      id: "registry",
      title: "注册表",
      desc: "重置 system.reg、user.reg 与 userdef.reg，下次启动自动从内置模板恢复干净注册表",
    },
    {
      id: "windows",
      title: "系统目录与额外运行库",
      desc: "重建 drive_c/windows，移除额外装入 system32 的运行库（如 MFC），下次自动重链 Wine 模块",
    },
    {
      id: "programs",
      title: "C 盘程序目录",
      desc: "清理 Program Files、Program Files (x86) 与 ProgramData",
    },
    {
      id: "users",
      title: "用户数据与存档（drive_c/users）",
      desc: "清理所有游戏的 C 盘文档、AppData 与存档文件，清理后无法恢复",
      badge: "含存档",
      badgeKind: "danger",
      danger: true,
    },
  ];

  let selected = $state<Record<WineCleanupTarget, boolean>>({
    temp: false,
    registry: false,
    windows: false,
    programs: false,
    users: false,
  });
  let initialized = $state(false);
  let loading = $state(true);
  let busy = $state(false);
  let problem = $state("");
  let notice = $state("");
  let dialog: HTMLDialogElement;
  let dialogOpen = $state(false);

  const status = $derived(wineEnv.status);

  function bytesFor(id: WineCleanupTarget): number {
    if (!status) return 0;
    if (id === "temp") return status.tempBytes;
    if (id === "registry") return status.registryBytes;
    if (id === "windows") return status.windowsBytes;
    if (id === "programs") return status.programsBytes;
    return status.usersBytes;
  }

  function titleFor(id: WineCleanupTarget): string {
    return t(OPTIONS.find((item) => item.id === id)?.title ?? id);
  }

  const chosen = $derived(
    OPTIONS.filter((item) => selected[item.id] && bytesFor(item.id) > 0),
  );
  const chosenBytes = $derived(
    chosen.reduce((sum, item) => sum + bytesFor(item.id), 0),
  );
  const hasDanger = $derived(chosen.some((item) => item.danger));
  const hasReset = $derived(
    chosen.some((item) => item.id === "registry" || item.id === "windows"),
  );
  const deferred = $derived(
    !!status?.serverStarted && chosen.some((item) => item.id !== "temp"),
  );

  onMount(() => {
    let active = true;
    loading = true;
    void wineEnv.refresh()
      .catch((error) => {
        if (active) problem = String(error);
      })
      .finally(() => {
        if (!active) return;
        loading = false;
        if (!initialized && (wineEnv.status?.tempBytes ?? 0) > 0) {
          selected.temp = true;
        }
        initialized = true;
      });
    return () => { active = false; };
  });

  $effect(() => {
    if (busy || dialogOpen) {
      return nav.onBack(() => {
        if (!busy && dialogOpen) dialog.close();
        return true;
      });
    }
  });

  function toggle(id: WineCleanupTarget) {
    if (busy || bytesFor(id) === 0) return;
    selected[id] = !selected[id];
    notice = "";
  }

  function openConfirm() {
    if (busy || !chosen.length) return;
    problem = "";
    dialog.showModal();
    dialogOpen = true;
  }

  function openDriveC() {
    if (busy) return;
    problem = "";
    if (preview) {
      notice = t("已打开 Windows C 盘");
      return;
    }
    void revealDrive().catch((error) => {
      problem = String(error);
    });
  }

  async function confirmClean() {
    if (busy || !chosen.length) return;
    busy = true;
    problem = "";
    notice = "";
    const targets = chosen.map((item) => item.id);
    try {
      const result = await wineEnv.clean(targets);
      for (const id of targets) selected[id] = false;
      dialog.close();
      if (result) {
        if (result.scheduled.length && result.executed.length) {
          notice = t("已清理临时文件，其余 {0} 项将在下次启动时执行。", result.scheduled.length);
        } else if (result.scheduled.length) {
          notice = t("已安排 {0} 项在下次启动时执行。", result.scheduled.length);
        } else {
          notice = t("已完成清理，释放 {0}。", storageSize(result.freedBytes));
        }
      }
    } catch (error) {
      problem = String(error);
    } finally {
      busy = false;
    }
  }

  async function cancelPending() {
    if (busy) return;
    busy = true;
    problem = "";
    try {
      await wineEnv.cancelPending();
      notice = t("已取消重启后的重置计划。");
    } catch (error) {
      problem = String(error);
    } finally {
      busy = false;
    }
  }

  async function restartNow() {
    if (busy) return;
    problem = "";
    try {
      await wineEnv.restart();
    } catch (error) {
      problem = String(error);
    }
  }
</script>

{#if device.phone}<PhoneSettingsHeader />{/if}
<main class="scroll">
  <div class="page">
    {#if !device.phone}
      <button class="quiet back" onclick={() => { if (!nav.requestBack()) nav.back(); }}>
        <Icon name="chevron-left" size={15} />{t("返回")}
      </button>
    {/if}
    <h1>{t("Wine 环境")}</h1>
    {#if problem}<p role="alert" class="destructive">{problem}</p>{/if}
    {#if loading && !status}<p role="status" class="sub">{t("正在读取文件夹…")}</p>{/if}

    {#if status}
      <div class="summary">
        <strong>{storageSize(status.totalBytes)}</strong>
        <button class="quiet" disabled={busy} onclick={openDriveC}>{t("打开 Windows C 盘")}</button>
      </div>

      {#if status.pendingReset.length}
        <section class="pending">
          <div class="pending-title">{t("已安排在下次启动时重置")}</div>
          <p class="sub">{t("重新打开 Hitode 后将自动执行：{0}", status.pendingReset.map(titleFor).join("、"))}</p>
          <div class="pending-actions">
            {#if device.platform === "macos" && !preview}
              <button class="primary" disabled={busy} onclick={() => void restartNow()}>{t("立即重启")}</button>
            {/if}
            <button class="secondary" disabled={busy} onclick={() => void cancelPending()}>{t("取消计划")}</button>
          </div>
        </section>
      {/if}

      {#if notice}
        <p role="status" class="status-note">{notice}</p>
      {/if}

      <div class="items">
        {#each OPTIONS as item (item.id)}
          {@const bytes = bytesFor(item.id)}
          {@const empty = bytes === 0}
          {@const checked = selected[item.id] && !empty}
          <button
            type="button"
            class="item"
            class:empty
            role="checkbox"
            aria-checked={checked}
            disabled={busy || empty}
            onclick={() => toggle(item.id)}
          >
            <span class="check" class:checked class:danger={checked && item.danger} aria-hidden="true">
              {#if checked}<Icon name="check" size={12} />{/if}
            </span>
            <span class="info">
              <span class="head">
                <strong class:destructive={item.danger}>{t(item.title)}</strong>
                {#if item.badge}
                  <span class="badge" class:danger-badge={item.badgeKind === "danger"}>{t(item.badge)}</span>
                {/if}
              </span>
              <span class="desc">{t(item.desc)}</span>
            </span>
            <span class="size">{empty ? t("已清空") : storageSize(bytes)}</span>
          </button>
        {/each}
      </div>

      <div class="footer">
        <div class="footer-info">
          {#if chosen.length}
            <div class="selected-text">{t("已选 {0} 项 · {1}", chosen.length, storageSize(chosenBytes))}</div>
            {#if deferred}<div class="defer-note">{t("将在下次启动时生效")}</div>{/if}
          {:else}
            <div class="sub">{t("未选择项目")}</div>
          {/if}
        </div>
        <button
          class="primary"
          class:destructive-button={hasDanger}
          disabled={busy || !chosen.length}
          onclick={openConfirm}
        >
          {t(hasReset ? "重置所选" : "清理所选")}
        </button>
      </div>
    {/if}
  </div>
</main>

<dialog
  bind:this={dialog}
  onclose={() => { dialogOpen = false; }}
  aria-labelledby="wine-dialog-title"
  oncancel={(e) => { if (busy) e.preventDefault(); }}
>
  <h2 id="wine-dialog-title">
    {t(deferred ? "安排下次启动时重置？" : hasDanger ? "确认清理并包含用户存档？" : "确认清理所选项目？")}
  </h2>
  {#if problem}<p role="alert" class="destructive">{problem}</p>{/if}
  {#if busy}<p role="status" class="sub">{t("正在处理…")}</p>{/if}
  <ul class="confirm-list">
    {#each chosen as item (item.id)}
      <li>
        <span class:destructive={item.danger}>{t(item.title)}</span>
        <span class="size">{storageSize(bytesFor(item.id))}</span>
      </li>
    {/each}
  </ul>
  <p class="confirm-note" class:destructive={hasDanger}>
    {t(hasDanger
      ? "所选包含 C 盘用户目录，游戏存档将被永久删除且无法恢复。游戏库记录与游戏副本不受影响。"
      : "C 盘用户存档、游戏库记录与游戏副本均会保留。")}
  </p>
  <div class="actions">
    <button class="secondary" disabled={busy} onclick={() => dialog.close()}>{t("取消")}</button>
    <button
      class="primary"
      class:destructive-button={hasDanger}
      disabled={busy}
      onclick={() => void confirmClean()}
    >
      {t(deferred ? "确认安排" : "确认执行")}
    </button>
  </div>
</dialog>

<style>
  main { flex: 1 1 0; min-height: 0; overflow: auto; }
  .page { box-sizing: border-box; max-width: 760px; margin: 0 auto; padding: 26px 28px 40px; }
  h1 { margin: 0 0 18px; font-size: 25px; }
  h2 { margin: 0 0 12px; font-size: 17px; }
  .back { display: inline-flex; align-items: center; gap: 3px; margin: 0 0 10px -7px; }
  .summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding-bottom: 16px;
    border-bottom: 1px solid var(--line);
  }
  .summary strong { font-size: 24px; font-variant-numeric: tabular-nums; }
  .summary .quiet {
    height: 32px;
    padding: 0 12px;
    border-radius: 16px;
    display: inline-flex;
    align-items: center;
    white-space: nowrap;
  }
  .pending {
    margin-top: 16px;
    padding: 14px 16px;
    border-radius: 12px;
    background: var(--field);
  }
  .pending-title { font-size: 14px; font-weight: 500; }
  .pending p { margin: 4px 0 0; }
  .pending-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 12px; }
  .status-note {
    margin: 14px 0 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--field);
    color: var(--fg2);
    font-size: 13px;
  }
  .items { border-bottom: 1px solid var(--line); }
  .item {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 14px 4px;
    border-top: 1px solid var(--line);
    cursor: pointer;
    text-align: start;
  }
  .item:first-child { border-top: 0; }
  .item:not(:disabled):hover { background: var(--hover); }
  .item:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; border-radius: 8px; }
  .item.empty { opacity: 0.45; cursor: default; }
  .check {
    box-sizing: border-box;
    width: 18px;
    height: 18px;
    margin-top: 2px;
    flex: none;
    border-radius: 5px;
    border: 1.5px solid var(--fg3);
    display: grid;
    place-items: center;
    color: var(--on-accent);
    transition: background-color 0.15s ease, border-color 0.15s ease;
  }
  .check.checked { background: var(--accent); border-color: var(--accent); }
  .check.danger { background: var(--danger); border-color: var(--danger); }
  :global(:root[data-theme="dark"]) .check.danger { color: var(--bg); }
  .info { flex: 1; min-width: 0; display: grid; gap: 4px; }
  .head { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; font-size: 14px; }
  .head strong { font-weight: 500; }
  .desc { color: var(--fg3); font-size: 12px; line-height: 1.45; }
  .badge {
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--field);
    color: var(--fg2);
    font-size: 11px;
  }
  .danger-badge { color: var(--danger); }
  .size {
    flex: none;
    padding-top: 1px;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
    color: var(--fg3);
    font-size: 13px;
  }
  .footer {
    margin-top: 18px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .footer-info { min-width: 0; }
  .selected-text { font-size: 14px; font-weight: 500; font-variant-numeric: tabular-nums; }
  .defer-note { margin-top: 2px; font-size: 12px; color: var(--fg3); }
  .footer .primary { flex: none; white-space: nowrap; }
  .sub { color: var(--fg3); font-size: 13px; }
  .destructive { color: var(--danger); }
  .destructive-button { background: var(--danger); }
  :global(:root[data-theme="dark"]) .destructive-button { color: var(--bg); }
  .actions { display: flex; align-items: center; justify-content: flex-end; flex-wrap: wrap; gap: 10px; margin-top: 14px; }
  .confirm-list {
    list-style: none;
    margin: 0 0 12px;
    padding: 8px 12px;
    border-radius: 10px;
    background: var(--field);
    display: grid;
    gap: 6px;
    font-size: 13px;
  }
  .confirm-list li { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .confirm-note { margin: 0; font-size: 13px; color: var(--fg2); line-height: 1.5; }
  dialog {
    box-sizing: border-box;
    color: var(--fg);
    background: var(--pop);
    border: 1px solid var(--line);
    border-radius: 14px;
    padding: 18px;
    width: min(440px, calc(100% - 28px));
  }
  dialog::backdrop { background: #0005; }
  :global(:root[data-device="phone"]) .page {
    padding: 8px calc(20px + var(--safe-right)) calc(24px + var(--safe-bottom)) calc(20px + var(--safe-left));
  }
  :global(:root[data-device="phone"]) .summary .quiet {
    height: 36px;
    border-radius: 18px;
    font-size: 14px;
  }
  :global(:root[data-device="phone"]) .head { font-size: 15px; }
</style>
