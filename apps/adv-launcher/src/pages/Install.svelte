<script lang="ts">
  import ImportEncoding from "../components/ImportEncoding.svelte";
  import ImportFooter from "../components/ImportFooter.svelte";
  import ImportMini from "../components/ImportMini.svelte";
  import ImportSwitch from "../components/ImportSwitch.svelte";
  import Icon from "../components/Icon.svelte";
  import { InstallFlow } from "../lib/import-flow.svelte";
  import { nav } from "../lib/nav.svelte";

  const flow = new InstallFlow();
  const { source, title, disc } = flow;
</script>

{#if flow.step === "识别"}
  <div class="pane fade">
    <main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
      <div class="col">
        <header>
          <h1>添加游戏</h1>
          <div class="mono" style="margin-top: 8px">{source.path}</div>
        </header>

        <section class="card" aria-label="识别结果">
          <div style="display: flex; align-items: center; gap: 14px; padding: 18px 18px 16px">
            <span class="disc" aria-hidden="true">
              {#if disc}
                <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"><circle cx="8" cy="8" r="6.3"></circle><circle cx="8" cy="8" r="1.7"></circle><path d="M8 3.6a4.4 4.4 0 0 1 4.4 4.4"></path></svg>
              {:else}
                <Icon name="folder" size={20} stroke={1.2} />
              {/if}
            </span>
            <div style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
              <span class="mono" lang={disc ? undefined : "ja"} style="font-size: 15px; color: var(--fg)">{source.label}</span>
              <span style="font-size: 12px; color: var(--fg3)">{source.detail}</span>
            </div>
          </div>
          <div class="found">
            <span style="display: flex; color: var(--accent)"><Icon name="check" stroke={1.8} /></span>
            <span>找到安装程序 <span class="mono" style="font-size: inherit; color: var(--fg)">{source.setup}</span></span>
          </div>
        </section>

        <section class="list" aria-label="安装选项">
          <div class="rw">
            <span class="k" style="flex: 0 0 auto">安装位置</span>
            <span class="v mono" style="flex: 1 1 auto; min-width: 0; text-align: right" lang="ja">{flow.target}</span>
            <button class="quiet sm" style="margin-right: -12px" onclick={() => flow.changeTarget()}>更改…</button>
          </div>
          {#if disc}
            <div class="rw">
              <span class="k">游玩时保留光盘</span>
              <span class="v mono" style="color: var(--fg3)">{source.drive}</span>
              <ImportSwitch checked={flow.keep} label="游玩时保留光盘" onchange={(v) => (flow.keep = v)} />
            </div>
          {/if}
        </section>
      </div>
    </main>
    <ImportFooter width={600}>
      <button class="quiet" onclick={() => nav.back()}>取消</button>
      <button class="primary" onclick={() => flow.start()}>开始安装</button>
    </ImportFooter>
  </div>
{:else if flow.step === "安装中"}
  <!-- The installer runs over the whole window, sidebar included. -->
  <div class="run fade">
    <header class="bar">
      <h1 style="margin: 0; flex: 1; font-size: 14px; font-weight: 500">正在安装</h1>
      <button class="quiet sm" onclick={() => flow.cancelRun()}>取消安装</button>
    </header>
    <main style="flex: 1 1 0; min-height: 0; display: grid; place-items: center; padding: 24px">
      <button class="frame" onclick={() => flow.tapFrame()} aria-label="安装程序窗口">
        <div class="w98" lang="ja" aria-hidden="true">
          <div class="tb">
            <span style="flex: 1">セットアップ</span>
            <span class="cb"><svg width="8" height="8" viewBox="0 0 8 8"><path d="M1 6.5h5" stroke="#000" stroke-width="1.6"></path></svg></span>
            <span class="cb"><svg width="8" height="8" viewBox="0 0 8 8" fill="none"><rect x="1" y="1" width="6" height="5.5" stroke="#000"></rect><path d="M1 1.6h6" stroke="#000" stroke-width="1.4"></path></svg></span>
            <span class="cb" style="margin-left: 2px"><svg width="8" height="8" viewBox="0 0 8 8"><path d="M1.2 1.2l5.6 5.6M6.8 1.2L1.2 6.8" stroke="#000" stroke-width="1.5"></path></svg></span>
          </div>
          <div class="hd">
            <div style="flex: 1; display: flex; flex-direction: column; gap: 4px">
              <span style="font-weight: 600">{flow.finished ? "セットアップの完了" : "インストール中"}</span>
              <span style="padding-left: 14px">{flow.finished ? "すべてのファイルがインストールされました。" : "ファイルをコピーしています。しばらくお待ちください。"}</span>
            </div>
            <span style="width: 40px; height: 40px; flex: 0 0 auto; background: linear-gradient(135deg, #7fa7d9, #2b4f8f)"></span>
          </div>
          {#if !flow.finished}
            <div class="bd">
              <span>コピー中: {flow.file}</span>
              <div class="wbar"><span class="fill" style="width: {flow.pct}%"></span></div>
              <span style="align-self: flex-end">{flow.pct}%</span>
            </div>
          {:else}
            <div class="bd" style="padding-top: 26px">
              <span>セットアップが完了しました。</span>
              <span>「完了」をクリックしてセットアップを終了してください。</span>
            </div>
          {/if}
          <div class="ft">
            <span class="btn dis">&lt; 戻る(B)</span>
            <span class="btn" class:def={flow.finished} class:dis={!flow.finished}>{flow.finished ? "完了" : "次へ(N) >"}</span>
            <span class="btn" class:dis={flow.finished}>キャンセル</span>
          </div>
        </div>
      </button>
    </main>
  </div>
{:else}
  <div class="pane fade">
    <main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
      <div class="col">
        <header>
          <div style="display: flex; align-items: center; gap: 10px; margin-bottom: 14px">
            <span class="badge" aria-hidden="true"><svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5l3.2 3L13 4.5"></path></svg></span>
            <span style="font-size: 13px; color: var(--fg2)">安装完成</span>
          </div>
          <h1 lang="ja">{title}</h1>
          <div class="mono" lang="ja" style="margin-top: 8px">{flow.target} · {source.installedSize}</div>
        </header>

        <section class="list" aria-label="游戏信息">
          <div class="rw">
            <span class="k" id="i-exe">主程序</span>
            <ImportMini
              options={source.exes.map((name) => ({ value: name, label: name }))}
              value={flow.exe}
              labelledby="i-exe"
              mono
              onpick={(v) => flow.pickExe(v)}
            />
          </div>
          {#if disc}
            <div class="rw">
              <span class="k">光盘</span>
              <span class="v mono">{flow.discLine}</span>
            </div>
          {/if}
        </section>

        <ImportEncoding
          choices={flow.encs}
          value={flow.enc}
          compact
          onpick={(v) => flow.pickEnc(v)}
        />
      </div>
    </main>
    <ImportFooter width={600}>
      <button class="primary" onclick={() => flow.add()}>{flow.addedId ? "开始游戏" : "添加到游戏库"}</button>
    </ImportFooter>
  </div>
{/if}

<style>
  .pane {
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .run {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--fg);
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
  .found {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 46px;
    padding: 0 18px;
    border-top: 1px solid var(--line);
    font-size: 13px;
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
  .bar {
    flex: 0 0 auto;
    height: 52px;
    box-sizing: border-box;
    padding: 0 16px 0 24px;
    display: flex;
    align-items: center;
    gap: 12px;
    border-bottom: 1px solid var(--line);
  }
  .frame {
    all: unset;
    box-sizing: border-box;
    position: relative;
    display: block;
    width: 640px;
    height: 480px;
    border-radius: 12px;
    overflow: hidden;
    background: #1f2124;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.18), 0 0 0 1px var(--line);
    cursor: pointer;
  }
  .frame:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }
  .w98 {
    position: absolute;
    left: 92px;
    top: 72px;
    width: 456px;
    height: 336px;
    box-sizing: border-box;
    padding: 3px;
    background: #d4d0c8;
    box-shadow: inset -1px -1px #404040, inset 1px 1px #d4d0c8, inset -2px -2px #808080, inset 2px 2px #ffffff;
    font-family: "MS UI Gothic", var(--font-ja);
    font-size: 12px;
    color: #000;
    display: flex;
    flex-direction: column;
    text-align: left;
  }
  .tb {
    height: 20px;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 3px 0 6px;
    background: linear-gradient(90deg, #0a246a, #a6caf0);
    color: #ffffff;
    font-weight: 600;
  }
  .cb {
    width: 16px;
    height: 14px;
    box-sizing: border-box;
    display: grid;
    place-items: center;
    background: #d4d0c8;
    box-shadow: inset -1px -1px #404040, inset 1px 1px #ffffff, inset -2px -2px #808080;
    color: #000;
  }
  .hd {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 12px;
    height: 58px;
    padding: 0 14px 0 18px;
    background: #ffffff;
    border-bottom: 1px solid #808080;
    box-shadow: 0 1px 0 #ffffff;
  }
  .bd {
    flex: 1 1 auto;
    padding: 20px 26px 0 40px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .wbar {
    height: 18px;
    box-sizing: border-box;
    padding: 2px;
    background: #d4d0c8;
    box-shadow: inset 1px 1px #808080, inset -1px -1px #ffffff;
  }
  .fill {
    display: block;
    height: 100%;
    background: repeating-linear-gradient(90deg, #0a246a 0 8px, transparent 8px 10px);
    transition: width 0.12s linear;
  }
  .ft {
    flex: 0 0 auto;
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    padding: 10px;
    border-top: 1px solid #808080;
    box-shadow: inset 0 1px #ffffff;
  }
  .btn {
    width: 76px;
    height: 23px;
    box-sizing: border-box;
    display: grid;
    place-items: center;
    background: #d4d0c8;
    box-shadow: inset -1px -1px #404040, inset 1px 1px #ffffff, inset -2px -2px #808080, inset 2px 2px #d4d0c8;
  }
  .btn.def {
    box-shadow: 0 0 0 1px #000, inset -1px -1px #404040, inset 1px 1px #ffffff, inset -2px -2px #808080, inset 2px 2px #d4d0c8;
  }
  .btn.dis {
    color: #808080;
    text-shadow: 1px 1px #ffffff;
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
    .fill {
      transition: none;
    }
    .fade {
      animation: none;
    }
  }
</style>
