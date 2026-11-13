/** Whether `path` lies on an Android external-storage volume (`/storage/emulated/<id>` or `/storage/<uuid>`). */
export function isAndroidExternalPath(path: string): boolean {
  return /^\/storage\/(?:emulated\/\d+|(?!emulated(?:\/|$)|self(?:\/|$))[^/]+)(?:\/|$)/.test(path);
}
