import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

// `--mode mock` swaps the Tauri bridge for fixtures so the UI can be built and
// reviewed as a single self-contained HTML file, with no Rust build involved.
export default defineConfig(({ mode }) => {
  const mock = mode === "mock";
  const local = (p: string) => fileURLToPath(new URL(p, import.meta.url));

  return {
    plugins: [svelte()],
    clearScreen: false,
    base: mock ? "./" : "/",
    server: { port: 1420, strictPort: true },
    resolve: {
      alias: mock
        ? {
            "@tauri-apps/api/core": local("./src/mock/core.ts"),
            "@tauri-apps/api/event": local("./src/mock/event.ts"),
          }
        : {},
    },
    build: {
      target: "safari15",
      outDir: mock ? "dist-mock" : "dist",
      emptyOutDir: true,
      // A classic (non-module) bundle so the preview opens over file://.
      rollupOptions: mock
        ? { output: { format: "iife", inlineDynamicImports: true } }
        : {},
    },
  };
});
