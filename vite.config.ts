import { tanstackStart } from "@tanstack/react-start/plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Static SPA output for the Tauri shell. Runtime mail work goes through native
// commands, so no Start server functions are used.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  plugins: [
    tanstackStart({
      // Tauri serves index.html for unknown paths, so deep-link reloads land on the shell.
      spa: { enabled: true, prerender: { outputPath: "/index.html" } },
    }),
    react(),
  ],
});
