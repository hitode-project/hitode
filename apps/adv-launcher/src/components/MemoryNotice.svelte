<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { t } from "../lib/i18n.svelte";
  import { inTauri } from "../lib/bridge";
  import { jit } from "../lib/jit.svelte";
  import { prefs, savePrefs } from "../lib/prefs.svelte";

  /** The guide's answer, which tells both installations apart in full. */
  const GUIDE = "https://github.com/hitode-project/hitode/blob/master/docs/ios.zh-CN.md#未开启内存扩展";

  let dialog = $state<HTMLElement>();
  $effect(() => dialog?.focus());

  function close(): void {
    prefs.memoryNoticeSeen = true;
    savePrefs();
  }

  function guide(): void {
    if (!inTauri) window.open(GUIDE, "_blank", "noopener");
    else void invoke("plugin:opener|open_url", { url: GUIDE, with: "inAppBrowser" }).catch(() => {});
  }
</script>

<!-- iOS, once: installations from before IOS-MEM-4 lack the memory extension. -->
<div class="backdrop">
  <div
    bind:this={dialog}
    role="alertdialog"
    aria-modal="true"
    aria-labelledby="memory-notice-title"
    tabindex="-1"
    onkeydown={(event) => { if (event.key === "Escape") close(); }}
  >
    <h2 id="memory-notice-title">{t("需要开启内存扩展")}</h2>
    <p>{t("当前安装没有开启 iOS 的内存扩展，游戏可能因为地址空间不足而无法启动。用旧版本的安装包或旧版 iloader 装的 Hitode 都需要重新安装一次。")}</p>
    {#if jit.status?.liveContainer}
      <p>{t("Hitode 装在 LiveContainer 里，用的是 LiveContainer 的签名：请在电脑上用 iloader 2.4.1 或更新版本覆盖安装 LiveContainer + SideStore，不要先删除 LiveContainer。Hitode 本身不用重装。")}</p>
    {:else}
      <p>{t("请在电脑上用 iloader 2.4.1 或更新版本的“导入 IPA”覆盖安装最新的 Hitode。不要先删除 Hitode，否则写在 C 盘的存档会一起删除。")}</p>
    {/if}
    <div class="actions">
      <button type="button" class="quiet" onclick={guide}>{t("查看说明")}</button>
      <button type="button" class="primary" onclick={close}>{t("知道了")}</button>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; z-index: 100; background: #0005; display: grid; place-items: center; }
  [role="alertdialog"] { background: var(--pop); color: var(--fg); padding: 24px; border-radius: 12px; width: min(420px, 85vw); outline: none; }
  h2 { margin: 0 0 12px; }
  p { margin: 0 0 8px; line-height: 1.5; }
  .actions { display: flex; justify-content: end; gap: 8px; margin-top: 20px; }
</style>
