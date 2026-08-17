import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

function hevcRuntimeAssets(): Plugin {
  const packageRoot = resolve(process.cwd(), "node_modules/@hevcjs/core/dist");
  const assets = [
    ["transcode-worker.js", "hevc/transcode-worker.js"],
    ["wasm/hevc-decode.js", "hevc/hevc-decode.js"],
    ["wasm/hevc-decode.wasm", "hevc/hevc-decode.wasm"],
  ] as const;

  return {
    name: "lux-hevc-runtime-assets",
    generateBundle() {
      for (const [source, fileName] of assets) {
        this.emitFile({ type: "asset", fileName, source: readFileSync(resolve(packageRoot, source)) });
      }
    },
  };
}

export default defineConfig({
  plugins: [react(), tailwindcss(), hevcRuntimeAssets()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    proxy: {
      "/api": process.env.LUX_API_ORIGIN ?? "http://127.0.0.1:8097",
      "/health": process.env.LUX_API_ORIGIN ?? "http://127.0.0.1:8097",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        entryFileNames: "assets/lux.js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
});
