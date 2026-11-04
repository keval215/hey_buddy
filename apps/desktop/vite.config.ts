import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  build: {
    rollupOptions: {
      input: {
        main: "index.html",
        overlay: "overlay.html",
        preview: "preview.html",
        approval: "approval.html",
      },
    },
  },
  server: { port: 1420, strictPort: true },
});
