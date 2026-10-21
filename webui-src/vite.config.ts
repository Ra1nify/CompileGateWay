import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 静态资源由 Rust 守护进程从站点根目录提供(server.rs),所以 base 保持 "/"。
// 产物直接写进 ../webui/dist,交给 Rust 侧的静态资源路由。
export default defineConfig({
  base: "/",
  plugins: [react()],
  build: {
    outDir: "../webui/dist",
    emptyOutDir: true,
    sourcemap: false,
    // 让 CSS 单独产出一个 /assets/*.css 文件(index.html 里会有 <link>)。
    cssCodeSplit: false,
    chunkSizeWarningLimit: 2048,
  },
  server: {
    port: 5273,
    strictPort: false,
  },
});
