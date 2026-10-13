import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import("@sveltejs/vite-plugin-svelte").SvelteConfig} */
export default {
  preprocess: vitePreprocess(),
  // Runes everywhere: `export let`, `$:` and other legacy syntax are compile errors.
  compilerOptions: { runes: true },
};
