import { Channel } from "@tauri-apps/api/core";
import { chooseFolder, setFolderChoiceBytes } from "./folder-choice.svelte";
import { hostInfo, inTauri, invokeHost } from "./bridge";

export interface StorageStatus {
  granted: boolean;
  supported: boolean;
}

/** Phones pick folders through the storage plugin: SAF on Android, Files on iOS. */
async function hostPlatform(): Promise<string> {
  return inTauri ? (await hostInfo()).platform : "";
}

async function isMobile(): Promise<boolean> {
  const os = await hostPlatform();
  return os === "android" || os === "ios";
}

export async function fetchStorageStatus(): Promise<StorageStatus> {
  return invokeHost<StorageStatus>("plugin:storage|storage_status");
}

export async function invokeRequestStorage(open = false): Promise<StorageStatus> {
  return invokeHost<StorageStatus>("plugin:storage|request_storage", { open });
}

export interface CopyProgress { bytes: number; file: string }
export interface FolderPickOptions {
  signal: AbortSignal;
  onSize: (bytes: number) => void;
  onProgress: (progress: CopyProgress) => void;
}
interface Folder {
  uri: string | null;
  path: string | null;
  managed?: boolean;
  canGrant?: boolean;
}

let preparedFolder: string | null = null;
let copyableUri: string | null = null;

/** Consume the permission sheet's folder without opening a second system picker. */
export function takePreparedFolder(): string | null {
  const folder = preparedFolder;
  preparedFolder = null;
  return folder;
}

/** Consume the SAF URI of the last in-place external folder pick for optional copying. */
export function takeCopyableUri(): string | null {
  const uri = copyableUri;
  copyableUri = null;
  return uri;
}

async function runCopy(
  uri: string,
  options: FolderPickOptions,
  sizePromise: Promise<number>,
): Promise<string | null> {
  const size = await sizePromise.catch(() => 0);
  if (options.signal.aborted) return null;
  options.onSize(size);
  const progress = new Channel<CopyProgress>();
  progress.onmessage = options.onProgress;
  const id = crypto.randomUUID();
  const cancel = () => { void invokeHost("plugin:storage|cancel_copy", { id }); };
  options.signal.addEventListener("abort", cancel, { once: true });
  try {
    const result = await invokeHost<{ path: string }>("plugin:storage|copy_folder", { uri, id, progress });
    if (options.signal.aborted) return null;
    return result.path;
  } finally {
    options.signal.removeEventListener("abort", cancel);
  }
}

async function confirmAndCopy(
  uri: string,
  options: FolderPickOptions & { onStart?: () => void },
  sizePromise: Promise<number>,
  initialBytes: number | null,
): Promise<string | null> {
  let bytes = initialBytes;
  if (bytes === null) {
    void sizePromise.then(
      (n) => {
        bytes = n;
        setFolderChoiceBytes(n);
      },
      () => {
        bytes = -1;
        setFolderChoiceBytes(-1);
      },
    );
  }
  const choice = await chooseFolder(options.signal, "copy", bytes);
  if (choice !== "copy" || options.signal.aborted) return null;
  options.onStart?.();
  return runCopy(uri, options, sizePromise);
}

/** Pick a mobile game folder, prompting for permission or copy only when needed. */
export async function prepareFolder(
  copy: boolean,
  signal: AbortSignal,
  onSize: (bytes: number) => void,
  onProgress: (progress: CopyProgress) => void,
): Promise<boolean> {
  copyableUri = null;
  if (!(await isMobile())) return true;
  const folder = await invokeHost<Folder>("plugin:storage|pick_folder");
  if (!folder.uri || signal.aborted) return false;
  if (!copy && folder.path) {
    preparedFolder = folder.path;
    copyableUri = folder.managed ? null : folder.uri;
    return true;
  }

  let knownBytes: number | null = null;
  const sizePromise = invokeHost<number>("plugin:storage|folder_size", { uri: folder.uri });
  void sizePromise.then(
    (bytes) => {
      knownBytes = bytes;
      setFolderChoiceBytes(bytes);
    },
    () => {
      knownBytes = -1;
      setFolderChoiceBytes(-1);
    },
  );
  const options: FolderPickOptions = { signal, onSize, onProgress };

  if (!copy && folder.canGrant) {
    const choice = await chooseFolder(signal, "permission", knownBytes);
    if (!choice || signal.aborted) return false;
    if (choice === "granted") {
      const resolved = await invokeHost<{ path: string | null }>("plugin:storage|resolve_folder", { uri: folder.uri });
      if (signal.aborted) return false;
      if (resolved.path) {
        preparedFolder = resolved.path;
        copyableUri = folder.uri;
        return true;
      }
      const copiedPath = await confirmAndCopy(folder.uri, options, sizePromise, knownBytes);
      if (!copiedPath) return false;
      preparedFolder = copiedPath;
      return true;
    }
    const copiedPath = await runCopy(folder.uri, options, sizePromise);
    if (!copiedPath) return false;
    preparedFolder = copiedPath;
    return true;
  }

  const copiedPath = await confirmAndCopy(folder.uri, options, sizePromise, knownBytes);
  if (!copiedPath) return false;
  preparedFolder = copiedPath;
  return true;
}

/** Prompt to copy an already-picked external SAF folder into Hitode's private storage. */
export async function copyExternalFolder(
  uri: string,
  options: FolderPickOptions & { onStart?: () => void },
): Promise<string | null> {
  if (!(await isMobile())) return null;
  const sizePromise = invokeHost<number>("plugin:storage|folder_size", { uri });
  return confirmAndCopy(uri, options, sizePromise, null);
}

export async function pickStorageFolder(options?: FolderPickOptions): Promise<string | null | undefined> {
  if (!(await isMobile())) return undefined;
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
