import { useId, useState } from "react";
import type { SavedRecipient } from "../lib/native";

// Commas inside quoted display names or angle addresses do not end a mailbox.
function mailboxRange(value: string, cursor: number) {
  let quoted = false;
  let angle = false;
  let escaped = false;
  let start = 0;
  for (let i = 0; i < value.length; i++) {
    const char = value[i];
    if (escaped) { escaped = false; continue; }
    if (char === "\\" && quoted) { escaped = true; continue; }
    if (char === '"') quoted = !quoted;
    if (!quoted && char === "<") angle = true;
    if (!quoted && char === ">") angle = false;
    if (char === "," && !quoted && !angle) {
      if (i >= cursor) return { start, end: i };
      start = i + 1;
    }
  }
  return { start, end: value.length };
}

export function RecipientInput({ id, value, onChange, recipients, autoFocus, placeholder }: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  recipients: SavedRecipient[];
  autoFocus?: boolean;
  placeholder?: string;
}) {
  const listId = useId();
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [cursor, setCursor] = useState(value.length);
  const [active, setActive] = useState(0);
  const range = mailboxRange(value, cursor);
  const query = value.slice(range.start, range.end).trim().toLocaleLowerCase();
  const other = (value.slice(0, range.start) + value.slice(range.end)).toLowerCase();
  const selectedEmails = new Set(other.match(/[^\s<>(),;"\\]+@[^\s<>(),;"\\]+/g) ?? []);
  const matches = focused && !dismissed && query
    ? recipients.filter((recipient) =>
        !selectedEmails.has(recipient.email.toLowerCase()) &&
        `${recipient.name ?? ""} ${recipient.email}`.toLocaleLowerCase().includes(query),
      ).slice(0, 8)
    : [];
  const index = Math.min(active, Math.max(0, matches.length - 1));
  const choose = (recipient: SavedRecipient) => {
    const mailbox = recipient.name
      ? `${JSON.stringify(recipient.name)} <${recipient.email}>`
      : recipient.email;
    const prefix = value.slice(0, range.start);
    const suffix = value.slice(range.end);
    onChange(`${prefix}${prefix ? " " : ""}${mailbox}${suffix || ", "}`);
    setDismissed(true);
  };

  return (
    <div className="recipient-input">
      <input
        id={id}
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={matches.length > 0}
        aria-controls={matches.length ? listId : undefined}
        aria-activedescendant={matches.length ? `${listId}-${index}` : undefined}
        autoComplete="off"
        autoCapitalize="none"
        spellCheck={false}
        autoFocus={autoFocus}
        placeholder={placeholder}
        value={value}
        onFocus={() => { setFocused(true); setDismissed(false); }}
        onBlur={() => setFocused(false)}
        onSelect={(event) => setCursor(event.currentTarget.selectionStart ?? value.length)}
        onChange={(event) => {
          onChange(event.target.value);
          setCursor(event.target.selectionStart ?? event.target.value.length);
          setDismissed(false);
          setActive(0);
        }}
        onKeyDown={(event) => {
          if (!matches.length || event.metaKey || event.ctrlKey || event.altKey) return;
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            setDismissed(true);
          } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setActive((index + (event.key === "ArrowDown" ? 1 : -1) + matches.length) % matches.length);
          } else if (event.key === "Enter" || (event.key === "Tab" && !event.shiftKey)) {
            event.preventDefault();
            const recipient = matches[index];
            if (recipient) choose(recipient);
          }
        }}
      />
      {matches.length > 0 && (
        <ul id={listId} role="listbox" aria-label="Saved recipients" className="recipient-suggestions">
          {matches.map((recipient, option) => (
            <li
              id={`${listId}-${option}`}
              key={recipient.email}
              role="option"
              aria-selected={index === option}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(recipient)}
            >
              {recipient.name && <strong>{recipient.name}</strong>}
              <span>{recipient.email}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
