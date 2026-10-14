import { t } from "./i18n.svelte";
const DAY = 86_400_000;

/** "Brand · 2004" with whichever of the two is known; empty when neither is. */
export function bylineLabel(brand: string, year: number): string {
  return [brand, year > 0 ? String(year) : ""].filter(Boolean).join(" · ");
}

/** "昨天", "3 天前", "上周" … relative to `now`, or "未开始". */
export function lastPlayedLabel(at: number | null, now: number): string {
  if (at === null) return t("未开始");
  const days = Math.floor((now - at) / DAY);
  if (days <= 0) return t("今天");
  if (days === 1) return t("昨天");
  if (days < 7) return t("{0} 天前", days);
  if (days < 14) return t("上周");
  if (days < 30) return t("{0} 周前", Math.floor(days / 7));
  if (days < 365) return t("{0} 个月前", Math.floor(days / 30));
  return t("{0} 年前", Math.floor(days / 365));
}

export function playTimeLabel(minutes: number): string {
  if (minutes <= 0) return "—";
  if (minutes < 60) return t("{0} 分钟", minutes);
  const hours = minutes / 60;
  return t("{0} 小时", Number.isInteger(hours) || hours >= 10 ? Math.round(hours) : hours.toFixed(1));
}

export function codepageLabel(codepage: number): string {
  switch (codepage) {
    case 932: return t("日文");
    case 936: return t("简体中文");
    case 950: return t("繁体中文");
    case 949: return t("韩文");
    case 1252: return t("西欧");
    default: return t("代码页 {0}", codepage);
  }
}
