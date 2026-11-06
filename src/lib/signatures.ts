import type { SavedSignature, SendingSettings } from "./native";

/** Older accounts have just the default text. Keep it available as a named choice. */
export function savedSignatures(settings: SendingSettings | undefined): SavedSignature[] {
  if (settings?.signatures?.length) return settings.signatures;
  return settings?.signature
    ? [{ id: "default", name: "Default", body: settings.signature }]
    : [];
}
