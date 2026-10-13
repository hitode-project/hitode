<script lang="ts">
  import ImportEncoding from "../components/ImportEncoding.svelte";
  import ImportFooter from "../components/ImportFooter.svelte";
  import ImportOption from "../components/ImportOption.svelte";
  import Icon from "../components/Icon.svelte";
  import { addToLibrary, encChoices, IMPORT_CANDIDATE as game, type Enc, type Meta } from "../lib/fake-import";
  import { nav } from "../lib/nav.svelte";

  /** Index into matches, then the manual search / link row, then "no online data". */
  const LINK = game.matches.length;
  const NONE = LINK + 1;
  const VNDB = /(vndb\.org\/|^)v\d+/;

  let exe = $state(0);
  let enc = $state<Enc>("ja");
  let match = $state(0);
  let link = $state("");
  let res = $state(0);
  /** Library id once added; the button then starts the game. */
  let addedId = $state<string | null>(null);

  const encs = encChoices(game.title, game.mojibake);
  const linkOk = $derived(VNDB.test(link.trim()));
  const linkId = $derived(/v(\d+)/.exec(link)?.[1] ?? "");
  const searchOn = $derived(link.trim() !== "" && !linkOk);

  /** Any change after adding asks to add again with the new choices. */
  function edit(apply: () => void) {
    apply();
    addedId = null;
  }

  function chosenMeta(): Meta {
    const none: Meta = { title: game.title, brand: "", year: 0, cover: "" };
    if (match < LINK) return game.matches[match] ?? none;
    if (match === LINK && linkOk) return game.linked;
    if (match === LINK && searchOn) return game.results[res] ?? none;
    return none;
  }

  function add() {
    if (addedId) {
      nav.replace({ name: "launch", gameId: addedId });
      return;
    }
    addedId = addToLibrary(chosenMeta(), enc, game.folder);
  }

  const metaLine = (m: Meta) => `${m.brand} · ${m.year}`;
</script>

<main class="scroll" style="flex: 1 1 0; min-height: 0; overflow: auto">
  <div class="col">
    <header>
      <h1 lang="ja">{game.title}</h1>
      <div class="mono" style="margin-top: 8px">{game.folder} · {game.size}</div>
    </header>

    <fieldset>
      <legend class="lbl">主程序</legend>
      <div class="stack">
        {#each game.exes as x, i (x.name)}
          <ImportOption name="exe" checked={exe === i} onpick={() => edit(() => (exe = i))}>
            <span class="mono" style="font-size: 14px; color: var(--fg)">{x.name}</span>
            <span class="side-n">{x.size}</span>
          </ImportOption>
        {/each}
      </div>
    </fieldset>

    <ImportEncoding choices={encs} value={enc} onpick={(v) => edit(() => (enc = v))} />

    <fieldset>
      <legend class="lbl">资料</legend>
      <div class="stack">
        {#each game.matches as m, i (m.title)}
          <ImportOption name="match" checked={match === i} onpick={() => edit(() => (match = i))} style="min-height: 60px">
            <span class="th"><span class="art {m.cover}"></span></span>
            <span class="two">
              <span lang="ja" class="ell" style="font-size: 14px">{m.title}</span>
              <span lang="ja" class="sub">{metaLine(m)}</span>
            </span>
          </ImportOption>
        {/each}
        <ImportOption name="match" checked={match === LINK} onpick={() => edit(() => (match = LINK))} style="min-height: 60px">
          <span class="glyph"><Icon name="search" size={18} /></span>
          <span style="font-size: 14px">手动搜索或链接</span>
        </ImportOption>
        {#if match === LINK}
          <div class="linkbox">
            <label class="linkf">
              <input
                type="search"
                aria-label="搜索标题或粘贴 VNDB 链接"
                placeholder="搜索标题，或粘贴 VNDB 链接"
                value={link}
                oninput={(e) => edit(() => (link = e.currentTarget.value))}
              />
            </label>
            {#if linkOk}
              <div style="display: flex; align-items: center; gap: 14px; min-height: 52px">
                <span class="th"><span class="art {game.linked.cover}"></span></span>
                <span class="two">
                  <span lang="ja" style="font-size: 14px">{game.linked.title}</span>
                  <span class="sub"><span lang="ja">{game.linked.brand}</span> · {game.linked.year} · v{linkId}</span>
                </span>
              </div>
            {/if}
            {#if searchOn}
              <div class="stack">
                {#each game.results as r, i (r.title)}
                  <button class="res" class:on={res === i} aria-pressed={res === i} onclick={() => edit(() => (res = i))}>
                    <span class="th"><span class="art {r.cover}"></span></span>
                    <span class="two">
                      <span lang="ja" class="ell" style="font-size: 14px">{r.title}</span>
                      <span lang="ja" class="sub">{metaLine(r)}</span>
                    </span>
                    <span class="chk"><Icon name="check" stroke={1.8} /></span>
                  </button>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
        <ImportOption name="match" checked={match === NONE} onpick={() => edit(() => (match = NONE))} style="min-height: 60px">
          <span class="th"><span class="art cvx"></span></span>
          <span style="font-size: 14px">不使用在线资料</span>
        </ImportOption>
      </div>
    </fieldset>
  </div>
</main>

<ImportFooter width={600}>
  <button class="quiet" onclick={() => nav.back()}>取消</button>
  <button class="primary" onclick={add}>{addedId ? "开始游戏" : "添加到游戏库"}</button>
</ImportFooter>

<style>
  .col {
    max-width: 600px;
    margin: 0 auto;
    padding: 64px 32px 40px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 44px;
  }
  h1 {
    margin: 0;
    font-size: 30px;
    line-height: 1.25;
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  fieldset {
    all: unset;
    display: block;
  }
  .lbl {
    margin: 0 0 12px;
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
  .side-n {
    font-size: 12px;
    color: var(--fg3);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
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
  .sub {
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
    padding: 4px 16px 10px 60px;
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
    font-size: 13px;
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
    gap: 14px;
    min-height: 52px;
    padding: 0 14px;
    border-radius: 10px;
    cursor: pointer;
  }
  .res:hover {
    background: var(--hover);
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
    color: var(--accent);
    opacity: 0;
  }
  .res.on .chk {
    opacity: 1;
  }
</style>
