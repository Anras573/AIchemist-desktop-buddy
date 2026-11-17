import { resolve } from "node:path";
import { defineConfig } from "vite";

const here = import.meta.dirname;

// Each window is its own HTML entry, so the buddy window never loads
// code that belongs to another window (spec section 3).
export default defineConfig({
  root: resolve(here, "src"),
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: resolve(here, "dist"),
    emptyOutDir: true,
    target: "es2022",
    // The Tauri webviews support modulepreload natively.
    modulePreload: { polyfill: false },
    rollupOptions: {
      input: {
        buddy: resolve(here, "src/buddy/index.html"),
        settings: resolve(here, "src/settings/index.html"),
      },
    },
  },
});
