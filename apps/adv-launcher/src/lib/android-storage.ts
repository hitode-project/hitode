/** Whether `path` lies on an Android external-storage volume (`/storage/emulated/<id>` or `/storage/<uuid>`). */
export function isAndroidExternalPath(path: string): boolean {
  return /^\/storage\/(?:emulated\/\d+|(?!emulated(?:\/|$)|self(?:\/|$))[^/]+)(?:\/|$)/.test(path);
}

/** Whether a game cannot be read because Android all-files access is turned off. */
export function isAndroidPermissionBlocked(params: {
  platform: string;
  checked: boolean;
  supported: boolean;
  granted: boolean;
  path: string;
  offline: "disk" | "missing" | null;
}): boolean {
  return (
    params.platform === "android" &&
    params.checked &&
    params.supported &&
    !params.granted &&
    params.offline !== "disk" &&
    isAndroidExternalPath(params.path)
  );
}
