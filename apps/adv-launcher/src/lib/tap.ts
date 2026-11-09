/** A touch that turned into a drag or was cancelled cannot activate a button. */
export function tap(run: () => void) {
  let start: { id: number; x: number; y: number } | null = null;
  let cancelled = false;

  function track(event: PointerEvent) {
    if (start?.id === event.pointerId &&
        Math.hypot(event.clientX - start.x, event.clientY - start.y) > 10) {
      cancelled = true;
    }
  }

  return {
    onpointerdown(event: PointerEvent) {
      if (!event.isPrimary) return;
      start = event.pointerType === "touch" ? { id: event.pointerId, x: event.clientX, y: event.clientY } : null;
      cancelled = false;
    },
    onpointermove: track,
    onpointerup: track,
    onpointercancel(event: PointerEvent) {
      if (start?.id !== event.pointerId) return;
      cancelled = true;
      start = null;
    },
    onclick(event: MouseEvent) {
      if (!cancelled || event.detail === 0) run();
    },
  };
}
