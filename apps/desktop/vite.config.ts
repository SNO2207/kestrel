import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  // Keep Rust compiler output visible when Vite runs under `tauri dev`.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "esnext",
    // Two pages: the app's window, and the quick launcher's (launcher.rs).
    rolldownOptions: { input: { main: "index.html", quick: "quick.html" } },
  },
});
