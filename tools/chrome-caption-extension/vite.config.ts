import { copyFileSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { defineConfig } from "vite";

const root = resolve(import.meta.dirname);
const outDir = resolve(root, "dist");

export default defineConfig({
  root,
  resolve: {
    alias: {
      stream: resolve(root, "../../web/node_modules/stream-browserify/index.js"),
      util: resolve(root, "../../web/node_modules/util/util.js"),
      events: resolve(root, "../../web/node_modules/events/events.js"),
      buffer: resolve(root, "../../web/node_modules/buffer/index.js"),
      process: resolve(root, "../../web/node_modules/process/browser.js"),
    },
  },
  build: {
    outDir,
    emptyOutDir: true,
    target: "chrome120",
    rollupOptions: {
      input: {
        "service-worker": resolve(root, "src/service-worker.ts"),
        "content-script": resolve(root, "src/content-script.ts"),
      },
      output: {
        format: "es",
        entryFileNames: "[name].js",
        inlineDynamicImports: false,
      },
    },
  },
  plugins: [{
    name: "copy-extension-manifest",
    closeBundle() {
      mkdirSync(outDir, { recursive: true });
      copyFileSync(resolve(root, "manifest.json"), resolve(outDir, "manifest.json"));
    },
  }],
});
