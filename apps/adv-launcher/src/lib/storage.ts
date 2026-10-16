import { Channel, invoke } from "@tauri-apps/api/core";
import { hostInfo, inTauri } from "./bridge";
import { hasStoragePermission as fakeStatus, requestStoragePermission as fakeRequest } from "./fake-import";

/** Phones pick folders through the storage plugin: SAF on Android, Files on iOS. */
const mobile = inTauri ? hostInfo().then((info) => info.platform === "android" || info.platform === "ios") : Promise.resolve(false);

export async function hasStoragePermission(): Promise<boolean> {
  if (await mobile) return (await invoke<{ granted: boolean }>("plugin:storage|storage_status")).granted;
  return inTauri || fakeStatus();
}

export async function requestStoragePermission(): Promise<boolean> {
  if (await mobile) return (await invoke<{ granted: boolean }>("plugin:storage|request_storage")).granted;
  fakeRequest();
  return true;
}

export interface CopyProgress { bytes: number; file: string }
export interface FolderPickOptions {
  signal: AbortSignal;
  onSize: (bytes: number) => void;
  onProgress: (progress: CopyProgress) => void;
}
interface Folder { uri: string | null; path: string | null }

let preparedFolder: string | null = null;

/** Consume the permission sheet's folder without opening a second system picker. */
export function takePreparedFolder(): string | null {
  const folder = preparedFolder;
  preparedFolder = null;
  return folder;
}

/** Keep the permission sheet open while a picked folder copies, then hand its complete path to the scan. */
export async function prepareFolder(
  copy: boolean,
  signal: AbortSignal,
  onSize: (bytes: number) => void,
  onProgress: (progress: CopyProgress) => void,
): Promise<boolean> {
  if (!(await mobile)) return true;
  const folder = await invoke<Folder>("plugin:storage|pick_folder");
  if (!folder.uri || signal.aborted) return false;
  if (!copy && folder.path) {
    preparedFolder = folder.path;
    return true;
  }
  onSize(await invoke<number>("plugin:storage|folder_size", { uri: folder.uri }));
  if (signal.aborted) return false;
  const progress = new Channel<CopyProgress>();
  progress.onmessage = onProgress;
  const id = crypto.randomUUID();
  const cancel = () => { void invoke("plugin:storage|cancel_copy", { id }); };
  signal.addEventListener("abort", cancel, { once: true });
  try {
    const result = await invoke<{ path: string }>("plugin:storage|copy_folder", { uri: folder.uri, id, progress });
    if (signal.aborted) return false;
    preparedFolder = result.path;
    return true;
  } finally {
    signal.removeEventListener("abort", cancel);
  }
}

export async function pickStorageFolder(options?: FolderPickOptions): Promise<string | null | undefined> {
  if (!(await mobile)) return undefined;
  const { signal, onSize, onProgress } = options ?? {
    signal: new AbortController().signal, onSize: () => {}, onProgress: () => {},
  };
  return await prepareFolder(false, signal, onSize, onProgress) ? takePreparedFolder() : null;
}

export function storageSize(bytes: number): string {
  if (bytes >= 1073741824) return `${(bytes / 1073741824).toFixed(1)} GB`;
  if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${Math.ceil(bytes / 1024)} KB`;
}
