import { flushSync } from "svelte";
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
 * Where the launcher runs. Touch (Android, iOS, or `?phone` in the URL while
 * developing, which also simulates the status bar and home indicator insets)
 * decides input and the in-game overlay. The window decides the layout: the
 * phone layout below 600 px on a desktop, and on touch unless the window is
 * at least 760 × 600 px, so a split screen, a slide-over window or a folded
 * phone falls back to it while a tablet or an unfolded phone gets the sidebar.
 */
class Device {
  platform = $state(guessPlatform());
  version = $state("browser preview");
  /** A native game view is attached under the webview. */
  gameView = $state(false);
  readonly simulated = new URLSearchParams(location.search).has("phone");
  readonly touch = $derived(this.platform === "android" || this.platform === "ios" || this.simulated);
  #narrow = $state(false);
  #roomy = $state(false);
  readonly phone = $derived(this.touch ? !this.#roomy : this.#narrow);

  constructor() {
    const narrow = matchMedia("(max-width: 599px)");
    const roomy = matchMedia("(min-width: 760px) and (min-height: 600px)");
    this.#narrow = narrow.matches;
    this.#roomy = roomy.matches;
    const resize = () => this.#resize(narrow.matches, roomy.matches);
    narrow.addEventListener("change", resize);
    roomy.addEventListener("change", resize);
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

  /**
   * Applies a window size change, crossfading when it swaps the layout. The
   * game screen changes at once: it is transparent over the native game view.
   */
  #resize(narrow: boolean, roomy: boolean): void {
    const apply = () => {
      this.#narrow = narrow;
      this.#roomy = roomy;
    };
    const root = document.documentElement;
    const swaps = (this.touch ? !roomy : narrow) !== this.phone;
    if (!swaps || !document.startViewTransition || root.dataset.screen === "game") {
      apply();
      return;
    }
    root.dataset.motion = "fade";
    document.startViewTransition(() => {
      apply();
      flushSync();
    });
  }
}

export const device = new Device();
