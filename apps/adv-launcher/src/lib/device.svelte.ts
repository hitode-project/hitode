import { hostInfo } from "./bridge";

function guessPlatform(): string {
  const ua = navigator.userAgent;
  if (/Android/.test(ua)) return "android";
  if (/iPhone|iPad/.test(ua)) return "ios";
  if (/Mac/.test(ua)) return "macos";
  if (/Windows/.test(ua)) return "windows";
  return "linux";
}

/**
 * Where the launcher runs. The phone layout is used on Android and iOS, in a
 * window narrower than 600 px, and with `?phone` in the URL while developing
 * (which also simulates the status bar and home indicator insets).
 */
class Device {
  platform = $state(guessPlatform());
  version = $state("browser preview");
  /** A native game view is attached under the webview. */
  gameView = $state(false);
  readonly simulated = new URLSearchParams(location.search).has("phone");
  #narrow = $state(false);
  readonly phone = $derived(
    this.platform === "android" || this.platform === "ios" || this.simulated || this.#narrow,
  );

  constructor() {
    const query = matchMedia("(max-width: 599px)");
    this.#narrow = query.matches;
    query.addEventListener("change", () => (this.#narrow = query.matches));
    hostInfo().then(
      (info) => {
        this.platform = info.platform;
        this.version = info.version;
        this.gameView = info.gameView;
      },
      // Outside Tauri the user agent guess stands.
      () => {},
    );
  }
}

export const device = new Device();
