import { loadGameSetup, GAME_DEFAULTS } from "./fake-settings";
import { prefs } from "./prefs.svelte";
export function playback(gameId: string) {
  const setup = loadGameSetup(gameId, 932);
  const values = { scale: prefs.scale, ff: prefs.fastForward, ...GAME_DEFAULTS, ...setup.overrides };
  const fontFamilies: Record<string, string> = {};
  for (const [key, aliases] of [
    ["gothic", ["ＭＳ ゴシック", "ＭＳ Ｐゴシック", "MS Gothic", "MS PGothic"]],
    ["mincho", ["ＭＳ 明朝", "ＭＳ Ｐ明朝", "MS Mincho", "MS PMincho"]],
    ["chinese", ["SimSun", "NSimSun", "SimHei", "Microsoft YaHei", "Microsoft YaHei UI", "宋体", "新宋体", "黑体", "微软雅黑", "MingLiU", "PMingLiU", "Microsoft JhengHei", "Microsoft JhengHei UI", "細明體", "新細明體", "微軟正黑體"]],
  ] as const) {
    const choice = values[key];
    if (choice.startsWith("font:")) for (const name of aliases) fontFamilies[name] = choice.slice(5);
  }
  return {
    scale: values.scale, volume: prefs.volume, muted: prefs.muted,
    toggle: values.ff === "toggle", pauseInBackground: prefs.pauseInBackground,
    discs: setup.discs, fontFiles: setup.fontFiles, fontFamilies,
    smoothing: values.smooth === "game" ? null : values.smooth === "on",
  };
}
