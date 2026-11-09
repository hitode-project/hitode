type Choice = "original" | "copy" | null;
type Prompt = { bytes: number | null; forced: boolean; resolve: (choice: Choice) => void };

export const folderChoice = $state<{ prompt: Prompt | null }>({ prompt: null });

export function chooseFolder(
  signal: AbortSignal,
  bytes: number | null,
  forced = false,
): Promise<Choice> {
  if (signal.aborted) return Promise.resolve(null);
  return new Promise((resolve) => {
    const cancel = () => finish(null);
    function finish(choice: Choice) {
      signal.removeEventListener("abort", cancel);
      folderChoice.prompt = null;
      resolve(choice);
    }
    signal.addEventListener("abort", cancel, { once: true });
    folderChoice.prompt = { bytes, forced, resolve: finish };
  });
}
