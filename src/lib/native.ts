import { invoke } from "@tauri-apps/api/core";

// Typed wrappers for the narrowly scoped native commands. Secrets never cross
// this boundary; the backend returns status only.

export type Check = { name: string; ok: boolean; detail: string };
export type FoundationReport = { checks: Check[] };

export function isDesktop(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function foundationReport(): Promise<FoundationReport> {
  return invoke<FoundationReport>("foundation_report");
}

export function openViewerFixture(): Promise<void> {
  return invoke<void>("open_viewer_fixture");
}

export function openViewerProbe(): Promise<void> {
  return invoke<void>("open_viewer_probe");
}

export function quitApp(): Promise<void> {
  return invoke<void>("quit_app");
}
