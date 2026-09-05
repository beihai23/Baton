import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    // Kimi Work 预览会传入 --port/--host；此处仅设默认值
    port: 5173,
    proxy: {
      "/api": { target: "http://127.0.0.1:7700", changeOrigin: true },
    },
  },
});
