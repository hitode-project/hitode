const DAY = 86_400_000;

/** "Brand · 2004" with whichever of the two is known; empty when neither is. */
export function bylineLabel(brand: string, year: number): string {
  return [brand, year > 0 ? String(year) : ""].filter(Boolean).join(" · ");
}

/** "昨天", "3 天前", "上周" … relative to `now`, or "未开始". */
export function lastPlayedLabel(at: number | null, now: number): string {
  if (at === null) return "未开始";
  const days = Math.floor((now - at) / DAY);
  if (days <= 0) return "今天";
  if (days === 1) return "昨天";
  if (days < 7) return `${days} 天前`;
  if (days < 14) return "上周";
  if (days < 30) return `${Math.floor(days / 7)} 周前`;
  if (days < 365) return `${Math.floor(days / 30)} 个月前`;
  return `${Math.floor(days / 365)} 年前`;
}

export function playTimeLabel(minutes: number): string {
  if (minutes <= 0) return "—";
  if (minutes < 60) return `${minutes} 分钟`;
  const hours = minutes / 60;
  return `${Number.isInteger(hours) || hours >= 10 ? Math.round(hours) : hours.toFixed(1)} 小时`;
}

export function codepageLabel(codepage: number): string {
  switch (codepage) {
    case 932: return "日文";
    case 936: return "简体中文";
    case 950: return "繁体中文";
    case 949: return "韩文";
    case 1252: return "西欧";
    default: return `代码页 ${codepage}`;
  }
}
