<script lang="ts">
  import Icon from "../components/Icon.svelte";
  import PhoneImportEncoding from "../components/PhoneImportEncoding.svelte";
  import PhoneImportFooter from "../components/PhoneImportFooter.svelte";
  import PhoneImportHeader from "../components/PhoneImportHeader.svelte";
  import PhoneImportOption from "../components/PhoneImportOption.svelte";
  import PhoneImportPage from "../components/PhoneImportPage.svelte";
  import { ImportPick, metaLine } from "../lib/import-flow.svelte";
  import { nav } from "../lib/nav.svelte";

  const pick = new ImportPick();
  const { game, LINK, NONE } = pick;
</script>

<PhoneImportPage>
  <PhoneImportHeader label="取消添加" onclose={() => nav.back()} />

  <main class="scroll body">
    <div>
      <h1 lang="ja">{game.title}</h1>
      <div class="mono" style="margin-top: 6px">{game.folder} · {game.size}</div>
    </div>

    <fieldset>
      <legend class="lbl">主程序</legend>
      <div class="stack">
        {#each game.exes as x, i (x.name)}
          <PhoneImportOption name="exe" checked={pick.exe === i} onpick={() => pick.pickExe(i)}>
            <span class="mono" style="font-size: 14px; color: var(--fg)">{x.name}</span>
            <span style="font-size: 12px; color: var(--fg3)">{x.size}</span>
          </PhoneImportOption>
        {/each}
      </div>
    </fieldset>

    <PhoneImportEncoding choices={pick.encs} value={pick.enc} onpick={(v) => pick.pickEnc(v)} />

    <fieldset>
      <legend class="lbl">资料</legend>
      <div class="stack">
        {#each game.matches as m, i (m.title)}
          <PhoneImportOption name="match" checked={pick.match === i} onpick={() => pick.pickMatch(i)} style="min-height: 60px">
            <span class="th"><span class="art {m.cover}"></span></span>
            <span class="two">
              <span lang="ja" class="ell" style="font-size: 14px">{m.title}</span>
              <span lang="ja" class="meta">{metaLine(m)}</span>
            </span>
          </PhoneImportOption>
        {/each}
        <PhoneImportOption name="match" checked={pick.match === LINK} onpick={() => pick.pickMatch(LINK)} style="min-height: 60px">
          <span class="glyph"><Icon name="search" size={18} /></span>
          <span style="font-size: 14px">手动搜索或链接</span>
        </PhoneImportOption>
        {#if pick.match === LINK}
          <div class="linkbox">
            <label class="linkf">
              <input
                type="search"
                aria-label="搜索标题或粘贴 VNDB 链接"
                placeholder="搜索标题，或粘贴 VNDB 链接"
                value={pick.link}
                oninput={(e) => pick.setLink(e.currentTarget.value)}
              />
            </label>
            {#if pick.linkOk}
              <div style="display: flex; align-items: center; gap: 12px; min-height: 52px; padding: 0 14px">
                <span class="th"><span class="art {game.linked.cover}"></span></span>
                <span class="two">
                  <span lang="ja" style="font-size: 14px">{game.linked.title}</span>
                  <span class="meta"><span lang="ja">{game.linked.brand}</span> · {game.linked.year} · v{pick.linkId}</span>
                </span>
              </div>
            {/if}
            {#if pick.searchOn}
              <div class="stack">
                {#each game.results as r, i (r.title)}
                  <button class="res" class:on={pick.res === i} aria-pressed={pick.res === i} onclick={() => pick.pickRes(i)}>
                    <span class="th"><span class="art {r.cover}"></span></span>
                    <span class="two">
                      <span lang="ja" class="ell" style="font-size: 14px">{r.title}</span>
                      <span lang="ja" class="meta">{metaLine(r)}</span>
                    </span>
                    <span class="chk"><Icon name="check" stroke={1.8} /></span>
                  </button>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
        <PhoneImportOption name="match" checked={pick.match === NONE} onpick={() => pick.pickMatch(NONE)} style="min-height: 60px">
          <span class="th"><span class="art cvx"></span></span>
          <span style="font-size: 14px">不使用在线资料</span>
        </PhoneImportOption>
      </div>
    </fieldset>
  </main>

  <PhoneImportFooter>
    <button class="primary" onclick={() => pick.add()}>{pick.addedId ? "开始游戏" : "添加到游戏库"}</button>
  </PhoneImportFooter>
</PhoneImportPage>

<style>
  .body {
    flex: 1 1 0;
    min-height: 0;
    overflow: auto;
    padding: 8px calc(var(--safe-right) + 20px) calc(var(--safe-bottom) + 86px) calc(var(--safe-left) + 20px);
    display: flex;
    flex-direction: column;
    gap: 32px;
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
  fieldset {
    all: unset;
    display: block;
  }
  .lbl {
    margin: 0 0 10px;
    padding: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--fg3);
    letter-spacing: 0.06em;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .th {
    position: relative;
    flex: 0 0 auto;
    width: 30px;
    aspect-ratio: 5 / 7;
    border-radius: 4px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--line);
  }
  .art {
    position: absolute;
    inset: 0;
  }
  .cvx {
    background: var(--field);
  }
  .two {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .meta {
    font-size: 12px;
    color: var(--fg3);
  }
  .ell {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .glyph {
    flex: 0 0 auto;
    width: 30px;
    display: grid;
    place-items: center;
    color: var(--fg2);
  }
  .linkbox {
    padding: 4px 0 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .linkf {
    display: flex;
    align-items: center;
    height: 44px;
    padding: 0 14px;
    border-radius: 10px;
    background: var(--field);
  }
  .linkf:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .linkf input {
    all: unset;
    flex: 1;
    min-width: 0;
    font-size: 14px;
    color: var(--fg);
  }
  .linkf input::placeholder {
    color: var(--fg3);
  }
  .res {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 52px;
    padding: 0 14px;
    border-radius: 10px;
    cursor: pointer;
  }
  .res.on {
    background: var(--sel);
  }
  .res:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .chk {
    flex: 0 0 auto;
    margin-left: auto;
    display: flex;
    color: var(--check);
    opacity: 0;
  }
  .res.on .chk {
    opacity: 1;
  }
</style>
