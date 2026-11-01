// Reads a stored From header, which is decoded but otherwise as sent.

export type Sender = { initials: string; tone: number; domain: string | null };

const SENDER_TONES = 8;

function initialsOf(text: string): string {
  const initials = text
    .split(/[\s._+-]+/)
    .map((word) => word.match(/[\p{L}\p{N}]/u)?.[0])
    .filter((initial) => initial !== undefined);
  const chosen =
    initials.length > 1 ? [initials[0], initials[initials.length - 1]] : initials;
  return chosen.join("").toLocaleUpperCase();
}

/** The same sender always gets the same tone, however they are named. */
function toneOf(text: string): number {
  let hash = 0;
  for (let index = 0; index < text.length; index++)
    hash = (Math.imul(hash, 31) + text.charCodeAt(index)) >>> 0;
  return hash % SENDER_TONES;
}

export function describeSender(sender: string): Sender {
  const value = sender.trim();
  const bracket = value.lastIndexOf("<");
  const address = (
    bracket >= 0 ? value.slice(bracket + 1).replace(/>.*$/, "") : value
  )
    .trim()
    .toLowerCase();
  const parts = address.match(/^([^\s@]+)@([^\s@]+)$/);
  const name = bracket >= 0 ? value.slice(0, bracket) : parts ? "" : value;
  return {
    initials: initialsOf(name) || initialsOf(parts?.[1] ?? "") || "?",
    tone: toneOf(parts ? address : value.toLowerCase()),
    domain: parts?.[2] ?? null,
  };
}
