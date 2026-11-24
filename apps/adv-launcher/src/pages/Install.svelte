<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import ImportEncoding from "../components/ImportEncoding.svelte";
  import ImportFooter from "../components/ImportFooter.svelte";
  import ImportMini from "../components/ImportMini.svelte";
  import ImportSwitch from "../components/ImportSwitch.svelte";
  import { device } from "../lib/device.svelte";
  import { InstallFlow } from "../lib/import-flow.svelte";
  import { nav } from "../lib/nav.svelte";

  let { gameId }: { gameId?: string | undefined } = $props();
  // svelte-ignore state_referenced_locally
  const flow = new InstallFlow(gameId ?? null);
</script>

{#if flow.step === "识别"}
  <div class="pane fade">
    <main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
      <div class="col">
        <header>
          <h1>{t("从光盘安装")}</h1>
          <div class="mono" style="margin-top: 8px">{flow.disc}</div>
        </header>

        <section class="card" aria-label={t("识别结果")}>
          <div style="display: flex; align-items: center; gap: 14px; padding: 18px 18px 16px">
            <span class="disc" aria-hidden="true">
              <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"><circle cx="8" cy="8" r="6.3"></circle><circle cx="8" cy="8" r="1.7"></circle><path d="M8 3.6a4.4 4.4 0 0 1 4.4 4.4"></path></svg>
            </span>
            <div style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
              <span class="mono" style="font-size: 15px; color: var(--fg)">{flow.label}</span>
              <span style="font-size: 12px; color: var(--fg3)">{flow.detail}</span>
            </div>
          </div>
        </section>

        <section class="list" aria-label={t("安装选项")}>
          <div class="rw">
            <span class="k" id="i-setup">{t("安装程序")}</span>
            <ImportMini options={flow.programs.map((p) => ({ value: p, label: p }))} value={flow.setup} labelledby="i-setup" mono onpick={(v) => (flow.setup = v)} />
          </div>
          <div class="rw">
            <span class="k" style="flex: 0 0 auto">{t("安装位置")}</span>
            <span class="v mono" style="flex: 1 1 auto; min-width: 0; text-align: right">{flow.target ?? t("Hitode 的游戏文件夹")}</span>
            {#if !device.touch}<button class="quiet sm" style="margin-right: -12px" onclick={() => void flow.changeTarget()}>{t("更改…")}</button>{/if}
          </div>
          <div class="rw">
            <span class="k">{t("游玩时保留光盘")}</span>
            <ImportSwitch checked={flow.keep} label={t("游玩时保留光盘")} onchange={(v) => (flow.keep = v)} />
          </div>
        </section>
        <ImportEncoding choices={flow.encs} value={flow.enc} compact onpick={(v) => flow.pickEnc(v)} />
      </div>
    </main>
    <ImportFooter width={600}>
      <button class="quiet" onclick={() => nav.back()}>{t("取消")}</button>
      <button class="primary" disabled={!flow.setup} onclick={() => void flow.start()}>{t("开始安装")}</button>
    </ImportFooter>
  </div>
{:else if flow.step === "完成"}
  <div class="pane fade">
    <main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
      <div class="col">
        <header>
          <div style="display: flex; align-items: center; gap: 10px; margin-bottom: 14px">
            <span class="badge" aria-hidden="true"><svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5l3.2 3L13 4.5"></path></svg></span>
            <span style="font-size: 13px; color: var(--fg2)">{t("安装程序已结束")}</span>
          </div>
          <h1 lang="ja">{flow.title}</h1>
          <div class="mono" lang="ja" style="margin-top: 8px">{flow.target}</div>
        </header>
        {#if flow.exes.length}
          <section class="list" aria-label={t("游戏信息")}>
            <div class="rw">
              <span class="k" id="i-exe">{t("主程序")}</span>
              <ImportMini options={flow.exes} value={flow.exe} labelledby="i-exe" mono onpick={(v) => flow.pickExe(v)} />
            </div>
          </section>
          <ImportEncoding choices={flow.encs} value={flow.enc} compact onpick={(v) => flow.pickEnc(v)} />
        {:else}
          <div class="status">{flow.error}</div>
        {/if}
      </div>
    </main>
    <ImportFooter width={600}>
      <button class="quiet" onclick={() => (gameId ? nav.replace({ name: "launch", gameId }) : nav.back())}>{t("再次运行安装程序")}</button>
      <button class="primary" disabled={!flow.exe} onclick={() => void flow.add()}>{t("开始游戏")}</button>
    </ImportFooter>
  </div>
{:else}
  <div class="pane fade">
    <div class="status" class:err={flow.step === "错误"} role="status">{flow.step === "错误" ? flow.error : t("正在读取光盘镜像…")}</div>
    <ImportFooter width={600}>
      <button class="quiet" onclick={() => nav.back()}>{t("返回")}</button>
    </ImportFooter>
  </div>
{/if}

<style>
  .status {
    flex: 1 1 0;
    padding: 96px 32px;
    text-align: center;
    font-size: 14px;
    color: var(--fg2);
  }
  .status.err {
    color: var(--danger);
  }
  .pane {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .col {
    max-width: 600px;
    margin: 0 auto;
    padding: 64px 32px 40px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 36px;
  }
  h1 {
    margin: 0;
    font-size: 30px;
    line-height: 1.25;
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  .card {
    display: flex;
    flex-direction: column;
    border-radius: 14px;
    background: var(--field);
  }
  .disc {
    flex: 0 0 auto;
    width: 44px;
    height: 44px;
    border-radius: 22px;
    display: grid;
    place-items: center;
    background: var(--sel);
    color: var(--fg2);
  }
  .list {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--line);
  }
  .rw {
    display: flex;
    align-items: center;
    gap: 16px;
    min-height: 60px;
    border-bottom: 1px solid var(--line);
  }
  .rw .k {
    flex: 1 1 auto;
    font-size: 14px;
  }
  .rw .v {
    font-size: 12px;
    color: var(--fg2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .quiet.sm {
    box-sizing: border-box;
    height: 32px;
    padding: 0 12px;
    display: flex;
    align-items: center;
    border-radius: 16px;
    font-size: 13px;
    white-space: nowrap;
  }
  .quiet.sm:focus-visible {
    outline-offset: 3px;
  }
  .badge {
    --ok: rgba(55, 103, 214, 0.1);
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--ok);
    color: var(--accent);
  }
  :global(:root[data-theme="dark"]) .badge {
    --ok: rgba(91, 134, 232, 0.16);
  }
  .fade {
    animation: fade 0.22s ease;
  }
  @keyframes fade {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fade {
      animation: none;
    }
  }
</style>
