import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const cluster = process.env.AMAFU_DASHBOARD ?? "http://127.0.0.1:8480";

export default defineConfig({
  plugins: [react()],
  css: { preprocessorOptions: { scss: { quietDeps: true, silenceDeprecations: ["import", "global-builtin", "color-functions", "mixed-decls", "if-function"] } } },
  build: {
    outDir: "../assets",
    emptyOutDir: true,
    assetsInlineLimit: 0,
    chunkSizeWarningLimit: 2048,
    rollupOptions: {
      output: {
        entryFileNames: "app.js",
        assetFileNames: (asset) => (asset.names?.[0]?.endsWith(".css") ? "app.css" : "fonts/[name][extname]"),
      },
    },
  },
  server: {
    proxy: {
      "/api": cluster,
      "/events": cluster,
      "/session": cluster,
    },
  },
});
