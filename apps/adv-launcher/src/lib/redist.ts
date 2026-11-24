export interface RuntimeComponent {
  id: string;
  name: string;
  dll: string;
}

const RUNTIME_COMPONENTS: readonly { id: string; name: string; dlls: readonly string[] }[] = [
  {
    id: "vc140-mfc",
    name: "Microsoft Visual C++ 2015–2022",
    dlls: ["mfc140u.dll", "mfc140.dll", "mfcm140u.dll", "mfcm140.dll"],
  },
];

/** Recognizes a missing DLL served by a known redistributable component. */
export function componentForDll(dll: string): RuntimeComponent | null {
  const lower = dll.trim().toLowerCase();
  for (const entry of RUNTIME_COMPONENTS) {
    if (entry.dlls.includes(lower)) {
      return { id: entry.id, name: entry.name, dll: dll.trim() };
    }
  }
  return null;
}

/** Extracts the known redistributable component from a host `missing_dll:` error. */
export function missingRuntimeComponent(message: string): RuntimeComponent | null {
  const missing = /^missing_dll:(.+)$/.exec(message);
  return missing?.[1] ? componentForDll(missing[1]) : null;
}

/**
 * Whether an unprompted exit presented zero frames and should check the game
 * folder for a registry script or `.reg` file before returning to the library.
 */
export function exitedWithoutFrame(
  event: { error?: string; summary?: { timeline?: { frames: number } | null } },
  stopped: boolean,
  hasSetup: boolean,
  isScript: boolean,
): boolean {
  if ("error" in event && event.error !== undefined) return false;
  if (stopped || hasSetup || isScript) return false;
  return event.summary?.timeline?.frames === 0;
}
