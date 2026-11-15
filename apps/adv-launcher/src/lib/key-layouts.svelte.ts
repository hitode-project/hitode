import {
  defaultProfiles,
  normalizeStoredKeyLayouts,
  type KeyProfile,
  type StoredKeyLayouts,
} from "./key-layouts";

const KEY = "adv-launcher.key-layouts";

function load(): StoredKeyLayouts {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return normalizeStoredKeyLayouts(JSON.parse(raw));
  } catch {
    // Storage may be unavailable; fall back to defaults.
  }
  return { profiles: defaultProfiles(), games: {} };
}

const store = $state<StoredKeyLayouts>(load());

export function saveKeyLayouts(): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(store));
  } catch {
    // Not persisted this session.
  }
}

export function keyProfiles(): KeyProfile[] {
  return store.profiles;
}

export function gameKeyProfileId(gameId: string): string | null {
  const id = store.games[gameId];
  return id && store.profiles.some((p) => p.id === id) ? id : null;
}

export function gameKeyProfile(gameId: string): KeyProfile | null {
  const id = gameKeyProfileId(gameId);
  return id ? (store.profiles.find((p) => p.id === id) ?? null) : null;
}

export function setGameKeyProfile(gameId: string, profileId: string | null): void {
  if (!profileId || !store.profiles.some((p) => p.id === profileId)) {
    delete store.games[gameId];
  } else {
    store.games[gameId] = profileId;
  }
  saveKeyLayouts();
}

export function forkKeyProfile(sourceId: string, name: string, gameId?: string): KeyProfile {
  const trimmed = name.trim().slice(0, 24);
  const source = store.profiles.find((p) => p.id === sourceId) ?? store.profiles[0] ?? defaultProfiles()[0]!;
  const id = `custom-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
  const copy: KeyProfile = {
    id,
    name: trimmed || `${source.name} 2`,
    opacity: source.opacity,
    autoDim: source.autoDim,
    items: $state.snapshot(source.items).map((it, idx) => ({
      ...it,
      id: `${id}-${idx}`,
    })),
  };
  store.profiles.push(copy);
  if (gameId) store.games[gameId] = copy.id;
  saveKeyLayouts();
  return store.profiles[store.profiles.length - 1]!;
}

export function deleteKeyProfile(profileId: string): void {
  const target = store.profiles.find((p) => p.id === profileId);
  if (!target || target.builtin) return;
  store.profiles = store.profiles.filter((p) => p.id !== profileId);
  const fallback = store.profiles[0]?.id;
  for (const [gid, pid] of Object.entries(store.games)) {
    if (pid === profileId) {
      if (fallback) store.games[gid] = fallback;
      else delete store.games[gid];
    }
  }
  saveKeyLayouts();
}
