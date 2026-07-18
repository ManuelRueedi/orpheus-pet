import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

// Tauri expects a fixed dev port and doesn't want Vite clearing the screen.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // index.html lives at the project root; frontend modules live in ./src.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      // The Rust side is rebuilt by Tauri, not Vite.
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    outDir: "dist", // Tauri's frontendDist points at ../dist
    target: "esnext",
    minify: false,
    sourcemap: true,
    rollupOptions: {
      // Two windows → two HTML entry points (the pet and its controls panel).
      input: {
        main: fileURLToPath(new URL("./index.html", import.meta.url)),
        panel: fileURLToPath(new URL("./panel.html", import.meta.url)),
      },
    },
  },
});
