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
    ["wasm/hevc-decode.js", "hevc/hevc-decode-module.js"],
    ["wasm/hevc-decode.wasm", "hevc/hevc-decode.wasm"],
  ] as const;

  const readAsset = (source: string, fileName: string) => {
    const contents = readFileSync(resolve(packageRoot, source));
    return fileName === "hevc/hevc-decode-module.js"
      ? Buffer.concat([contents, Buffer.from("\nexport default HEVCDecoderModule;\n")])
      : contents;
  };

  return {
    name: "lux-hevc-runtime-assets",
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const pathname = request.url?.split("?", 1)[0];
        const asset = assets.find(([, fileName]) => `/${fileName}` === pathname);
        if (!asset) {
          next();
          return;
        }
        response.statusCode = 200;
        response.setHeader("Content-Type", asset[1].endsWith(".wasm") ? "application/wasm" : "text/javascript");
        response.end(readAsset(asset[0], asset[1]));
      });
    },
    generateBundle() {
      for (const [source, fileName] of assets) {
        this.emitFile({ type: "asset", fileName, source: readAsset(source, fileName) });
      }
    },
  };
}

export default defineConfig({
  plugins: [react(), tailwindcss(), hevcRuntimeAssets()],
  resolve: {
    alias: {
      stream: resolve(process.cwd(), "node_modules/stream-browserify/index.js"),
      util: resolve(process.cwd(), "node_modules/util/util.js"),
      events: resolve(process.cwd(), "node_modules/events/events.js"),
      buffer: resolve(process.cwd(), "node_modules/buffer/index.js"),
      process: resolve(process.cwd(), "node_modules/process/browser.js"),
    },
  },
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
