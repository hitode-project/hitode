import { prefs } from "./prefs.svelte";
import { messages } from "./messages";

export function locale(): "zh" | "ja" | "en" {
  if (prefs.language !== "auto") return prefs.language;
  const system = navigator.language.toLowerCase();
  return system.startsWith("zh") ? "zh" : system.startsWith("ja") ? "ja" : "en";
}

export function t(source: string, ...values: unknown[]): string {
  const language = locale();
  const text = language === "zh" ? source : messages[source]?.[language === "en" ? 0 : 1] ?? source;
  return text.replace(/\{(\d+)\}/g, (_, index: string) => String(values[Number(index)]));
}
