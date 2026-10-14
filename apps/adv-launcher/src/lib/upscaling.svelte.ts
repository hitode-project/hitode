import { prefs, type Filter } from "./prefs.svelte";

const KEY = "adv-launcher.upscaling";
function load(): Record<string, Filter> {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? "{}");
  } catch {
    return {};
  }
}
const overrides = $state<Record<string, Filter>>(load());
export function filterOverride(id: string): Filter | undefined {
  return overrides[id];
}
export function gameFilter(id: string): Filter {
  return overrides[id] ?? prefs.filter;
}
export function setGameFilter(id: string, value: Filter | "inherit"): void {
  if (value === "inherit") delete overrides[id];
  else overrides[id] = value;
  localStorage.setItem(KEY, JSON.stringify(overrides));
}
