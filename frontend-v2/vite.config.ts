import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
    },
  },
  server: {
    port: 5175,
    proxy: {
      "/reader3": {
        target: "http://127.0.0.1:4605",
        changeOrigin: true,
      },
      "/assets": {
        target: "http://127.0.0.1:4605",
        changeOrigin: true,
      },
      "/epub": {
        target: "http://127.0.0.1:4605",
        changeOrigin: true,
      },
      "/health": {
        target: "http://127.0.0.1:4605",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    assetsDir: "static",
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) return undefined;
          if (
            /node_modules[\\/](react|react-dom|react-router|react-router-dom|scheduler|zustand|@tanstack)[\\/]/.test(
              id,
            )
          ) {
            return "vendor-react";
          }
          if (/node_modules[\\/](motion|framer-motion)[\\/]/.test(id)) {
            return "vendor-motion";
          }
          if (/node_modules[\\/](react-virtuoso|dompurify)[\\/]/.test(id)) {
            return "vendor-reader";
          }
          return undefined;
        },
      },
    },
  },
});
