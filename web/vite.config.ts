import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  // `make dev` serves the app from Vite (HMR) while the backend runs
  // separately on :7823. Proxy the WebSocket so the browser talks to one
  // origin and never hits CORS. Matches DEFAULT_WEB_ADDR in src/ui/web/mod.rs.
  server: {
    proxy: {
      "/ws": { target: "ws://127.0.0.1:7823", ws: true },
    },
  },
});
