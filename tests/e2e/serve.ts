// Serve the actual built SPA with the same index fallback used by Tauri.
// Browser tests must catch production-only hydration and asset failures.
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "../../dist/client");
Bun.serve({
  hostname: "127.0.0.1",
  port: Number(process.env.PIGEON_E2E_PORT ?? 1422),
  async fetch(request) {
    const pathname = decodeURIComponent(new URL(request.url).pathname);
    const path = resolve(root, `.${pathname}`);
    if (path !== root && !path.startsWith(`${root}/`)) {
      return new Response("Forbidden", { status: 403 });
    }
    const file = Bun.file(path);
    return new Response(
      path !== root && (await file.exists())
        ? file
        : Bun.file(`${root}/index.html`),
    );
  },
});
