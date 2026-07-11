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
  // separately on :7823. Proxy the WebSocket and the `/commands` REST
  // endpoint so the browser talks to one origin and never hits CORS —
  // without the `/commands` proxy the SPA fallback returns index.html and
  // the slash palette silently ends up empty. Matches DEFAULT_WEB_ADDR in
  // src/ui/web/mod.rs.
  server: {
    // Bind all interfaces so a phone on the same LAN can hit the dev server.
    host: true,
    // Pin the port so `make dev` is predictable — fail loudly if 5173 is
    // taken rather than silently drifting to 5174 (which leaves the URL we
    // print unreachable). `open` targets Vite's HMR port, not the backend.
    port: 5173,
    strictPort: true,
    open: true,
    proxy: {
      "/ws": { target: "ws://127.0.0.1:7823", ws: true },
      "/commands": { target: "http://127.0.0.1:7823" },
    },
  },
});
