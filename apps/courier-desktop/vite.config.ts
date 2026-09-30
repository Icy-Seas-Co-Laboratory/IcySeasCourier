import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import packageJson from "./package.json" with { type: "json" };

export default defineConfig({
  plugins: [svelte()],
  define: {
    __COURIER_VERSION__: JSON.stringify(packageJson.version),
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});
