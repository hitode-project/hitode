import { call, inTauri } from "./bridge";
import { library } from "./library.svelte";
import { prefs } from "./prefs.svelte";
import type { LibraryGame } from "./host";

class Scraper {
  busy = $state(false);
  error = $state("");
  current = $state("");
  #attempted = new Set<string>();
  async scan() {
    if (!inTauri || !prefs.vndbAsked || !prefs.fetchVndb || this.busy) return;
    this.busy = true;
    this.error = "";
    try {
      while (prefs.vndbAsked && prefs.fetchVndb) {
        const game = library.games.find((g) => !g.vndb && !g.vndbChecked && !this.#attempted.has(`${g.id}:${g.title}`));
        if (!game) break;
        this.#attempted.add(`${game.id}:${game.title}`);
        this.current = game.title;
        const entry = await call<LibraryGame | null>("vndb_auto", { id: game.id }, null);
        if (entry && library.get(entry.id)) library.put(entry);
      }
    } catch (error) { this.error = String(error); }
    finally { this.busy = false; this.current = ""; }
  }
  retry() { this.#attempted.clear(); void this.scan(); }
}
export const scraper = new Scraper();
