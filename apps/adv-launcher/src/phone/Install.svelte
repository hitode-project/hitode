<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  import Icon from "../components/Icon.svelte";
  import PhoneImportEncoding from "../components/PhoneImportEncoding.svelte";
  import PhoneImportFooter from "../components/PhoneImportFooter.svelte";
  import PhoneImportHeader from "../components/PhoneImportHeader.svelte";
  import PhoneImportPage from "../components/PhoneImportPage.svelte";
  import PhoneImportSwitch from "../components/PhoneImportSwitch.svelte";
  import { InstallFlow } from "../lib/import-flow.svelte";
  import { nav } from "../lib/nav.svelte";

  let { gameId }: { gameId?: string | undefined } = $props();
  // svelte-ignore state_referenced_locally
  const flow = new InstallFlow(gameId ?? null);
</script>

<PhoneImportPage>
  <PhoneImportHeader label={t("取消安装")} onclose={() => nav.back()} />
  <main class="scroll body fade">
    {#if flow.step === "识别"}
      <div>
        <h1>{t("从光盘安装")}</h1>
        <div class="mono ell" style="margin-top: 6px">{flow.disc}</div>
      </div>
      <section class="card" aria-label={t("识别结果")}>
        <div style="display: flex; align-items: center; gap: 12px; padding: 14px">
          <span class="disc" aria-hidden="true">
            <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"><circle cx="8" cy="8" r="6.3"></circle><circle cx="8" cy="8" r="1.7"></circle><path d="M8 3.6a4.4 4.4 0 0 1 4.4 4.4"></path></svg>
          </span>
          <div style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
            <span class="mono ell" style="font-size: 15px; color: var(--fg)">{flow.label}</span>
            <span style="font-size: 12px; color: var(--fg3)">{flow.detail}</span>
          </div>
        </div>
      </section>
      <section class="list" aria-label={t("安装选项")}>
        <div class="rw">
          <span class="k" id="p-i-setup">{t("安装程序")}</span>
          <span class="selw">
            <select class="mono-sel" aria-labelledby="p-i-setup" value={flow.setup} onchange={(e) => (flow.setup = e.currentTarget.value)}>
              {#each flow.programs as p (p)}
                <option value={p} selected={p === flow.setup}>{p}</option>
              {/each}
            </select>
            <Icon name="chevron-down" size={12} stroke={1.6} />
          </span>
        </div>
        <div class="rw">
          <span class="k">{t("光盘")}</span>
          <span class="v mono ell" style="flex: 1 1 auto; min-width: 0; text-align: right">{[flow.disc, ...flow.more].map((p) => p.split(/[\\/]/).pop()).join(" · ")}</span>
          {#if flow.canAddDisc}<button class="quiet" style="margin-right: -12px; flex: 0 0 auto" onclick={() => void flow.addDisc()}>{t("添加…")}</button>{/if}
        </div>
        <div class="rw">
          <span class="k">{t("游玩时保留光盘")}</span>
          <PhoneImportSwitch checked={flow.keep} label={t("游玩时保留光盘")} onchange={(v) => (flow.keep = v)} />
        </div>
      </section>
      <PhoneImportEncoding choices={flow.encs} value={flow.enc} compact onpick={(v) => flow.pickEnc(v)} />
    {:else if flow.step === "完成"}
      <div>
        <h1 lang="ja">{flow.title}</h1>
        <div class="hint" style="margin-top: 6px">{t("安装程序已结束")}</div>
      </div>
      {#if flow.exes.length}
        <section class="list" aria-label={t("游戏信息")}>
          <div class="rw">
            <span class="k" id="p-i-exe">{t("主程序")}</span>
            <span class="selw">
              <select class="mono-sel" aria-labelledby="p-i-exe" value={flow.exe} onchange={(e) => flow.pickExe(e.currentTarget.value)}>
                {#each flow.exes as item (item.value)}
                  <option value={item.value} selected={item.value === flow.exe}>{item.label}</option>
                {/each}
              </select>
              <Icon name="chevron-down" size={12} stroke={1.6} />
            </span>
          </div>
        </section>
        <PhoneImportEncoding choices={flow.encs} value={flow.enc} compact onpick={(v) => flow.pickEnc(v)} />
      {:else}
        <div class="status">{flow.error}</div>
      {/if}
    {:else}
      <div class="status" class:err={flow.step === "错误"} role="status">{flow.step === "错误" ? flow.error : t("正在读取光盘镜像…")}</div>
    {/if}
  </main>
  <PhoneImportFooter>
    {#if flow.step === "识别"}
      <button class="primary" disabled={!flow.setup} onclick={() => void flow.start()}>{t("开始安装")}</button>
    {:else if flow.step === "完成"}
      <button class="primary" disabled={!flow.exe} onclick={() => void flow.add()}>{t("开始游戏")}</button>
    {/if}
  </PhoneImportFooter>
</PhoneImportPage>

<style>
  .status {
    padding: 48px 0;
    text-align: center;
    font-size: 14px;
    color: var(--fg2);
  }
  .status.err {
    color: var(--danger);
  }
  .hint {
    font-size: 13px;
    color: var(--fg3);
  }
  .body {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 8px calc(var(--safe-right) + 20px) calc(var(--safe-bottom) + 86px) calc(var(--safe-left) + 20px);
    display: flex;
    flex-direction: column;
    gap: 28px;
    scrollbar-width: none;
  }
  .body::-webkit-scrollbar {
    display: none;
  }
  h1 {
    margin: 0;
    font-size: 26px;
    line-height: 1.3;
    font-weight: 600;
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
    justify-content: space-between;
    gap: 12px;
    min-height: 56px;
    padding: 6px 0;
    box-sizing: border-box;
    border-bottom: 1px solid var(--line);
  }
  .rw .k {
    flex: 0 0 auto;
    font-size: 15px;
    white-space: nowrap;
  }
  .rw .v {
    font-size: 13px;
    color: var(--fg2);
  }
  .selw {
    position: relative;
    flex: 0 1 auto;
    min-width: 0;
    max-width: 68%;
    display: inline-flex;
    align-items: center;
  }
  .selw select {
    appearance: none;
    -webkit-appearance: none;
    box-sizing: border-box;
    width: 100%;
    height: 40px;
    min-width: 128px;
    max-width: 100%;
    padding: 0 30px 0 12px;
    border: 0;
    border-radius: 10px;
    background: var(--field);
    color: var(--fg);
    font-size: 13px;
    text-overflow: ellipsis;
    overflow: hidden;
    white-space: nowrap;
    cursor: pointer;
  }
  .selw select.mono-sel {
    font-family: var(--font-mono);
  }
  .selw select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .selw :global(svg) {
    position: absolute;
    right: 11px;
    pointer-events: none;
    color: var(--fg3);
  }
  .ell {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .fade {
    animation: fade 0.22s ease;
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
  }
</style>
