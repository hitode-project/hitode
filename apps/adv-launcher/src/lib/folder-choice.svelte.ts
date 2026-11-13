export type FolderChoiceResult = "granted" | "copy" | null;
export type FolderChoiceMode = "permission" | "copy";

export interface FolderChoicePrompt {
  mode: FolderChoiceMode;
  /** Folder size in bytes while confirming copy; `null` while loading, `-1` if unavailable. */
  bytes: number | null;
  resolve: (choice: FolderChoiceResult) => void;
}

export const folderChoice = $state<{ prompt: FolderChoicePrompt | null }>({ prompt: null });

export function chooseFolder(
  signal: AbortSignal,
  mode: FolderChoiceMode,
  bytes: number | null,
): Promise<FolderChoiceResult> {
  if (signal.aborted) return Promise.resolve(null);
  return new Promise((resolve) => {
    const cancel = () => finish(null);
    function finish(choice: FolderChoiceResult) {
      signal.removeEventListener("abort", cancel);
      if (folderChoice.prompt?.resolve === finish) folderChoice.prompt = null;
      resolve(choice);
    }
    signal.addEventListener("abort", cancel, { once: true });
    folderChoice.prompt = { mode, bytes, resolve: finish };
  });
}

export function setFolderChoiceBytes(bytes: number): void {
  if (folderChoice.prompt && folderChoice.prompt.bytes === null) {
    folderChoice.prompt.bytes = bytes;
  }
}
