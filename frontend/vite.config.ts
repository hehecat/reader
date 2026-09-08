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
    port: 8082,
    proxy: {
      "/reader3": {
        target: "http://127.0.0.1:4396",
        changeOrigin: true,
      },
      "/assets": {
        target: "http://127.0.0.1:4396",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    // 后端 /assets 是 legacy storage 封面路由(nest_service), 前端产物必须避开
    assetsDir: "static",
    rollupOptions: {
      output: {
        // 按包分组的长效缓存 chunk; 未命中的 vendor 交给 Rollup 默认策略,
        // 页面代码由 App.tsx 的路由级动态 import 自动切分.
        manualChunks(id) {
          if (!id.includes("node_modules")) {
            return undefined;
          }
          // 核心运行时: 每个页面 chunk 都依赖
          if (
            /node_modules[\\/](react|react-dom|react-router|react-router-dom|scheduler|zustand|@tanstack)[\\/]/.test(
              id,
            )
          ) {
            return "vendor-core";
          }
          // Radix 无头组件(仅部分页面用到, 独立缓存)
          if (/node_modules[\\/]@radix-ui[\\/]/.test(id)) {
            return "vendor-radix";
          }
          // 阅读器重依赖: 虚拟列表 + HTML 消毒
          if (/node_modules[\\/](react-virtuoso|dompurify)[\\/]/.test(id)) {
            return "vendor-reader";
          }
          return undefined;
        },
      },
    },
  },
});
