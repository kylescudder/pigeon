import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Icon, type IconName } from "./icon";

// Only formatting we author may enter the privileged composer. In particular,
// restored drafts must not introduce images, remote resources, arbitrary
// styles or active content. Mirrors clean_composition in sending.rs.
const TAGS: Record<string, string> = {
  P: "p", DIV: "div", BR: "br", B: "b", STRONG: "strong", I: "i", EM: "em", U: "u", S: "s",
  STRIKE: "s", SUB: "sub", SUP: "sup", H1: "h1", H2: "h2", H3: "h3", BLOCKQUOTE: "blockquote",
  PRE: "pre", CODE: "code", HR: "hr", UL: "ul", OL: "ol", LI: "li", A: "a", SPAN: "span",
  // Colour commands write <font color>; it is kept as a styled span.
  FONT: "span",
};
const STYLED = new Set(["p", "div", "h1", "h2", "h3", "li", "ul", "ol", "blockquote", "span"]);
const DROPPED = new Set(["SCRIPT", "STYLE", "IFRAME", "OBJECT", "SVG", "MATH", "TEMPLATE", "HEAD", "TITLE"]);
// Named colours, but not the CSS-wide keywords Chrome scatters into "style spans".
const COLOR = /^(#[0-9a-f]{3,8}|rgba?\([\d\s,.%]{1,40}\)|(?!(initial|inherit|unset|revert|currentcolor)$)[a-z]{3,20})$/i;
const LENGTHS = /^(none|((0|[\d.]{1,6}(px|em))(\s+|$)){1,4})$/i;
const STYLES: Record<string, RegExp> = {
  color: COLOR,
  "background-color": COLOR,
  "text-align": /^(left|right|center|justify)$/i,
  // Indenting a paragraph produces a borderless, margined blockquote.
  margin: LENGTHS,
  "margin-left": LENGTHS,
  padding: LENGTHS,
  border: /^none$/i,
};

function cleanStyle(style: string) {
  return style.split(";").flatMap((declaration) => {
    const at = declaration.indexOf(":");
    const name = declaration.slice(0, at).trim().toLowerCase();
    const value = declaration.slice(at + 1).trim();
    return at > 0 && STYLES[name]?.test(value) ? [`${name}: ${value}`] : [];
  }).join("; ");
}

export function safeHref(href: string): string | null {
  const value = href.trim();
  return /^(https?:\/\/[^\s<>"]+|mailto:[^\s<>"]+)$/i.test(value) ? value : null;
}

export function cleanComposition(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const clean = (node: Node): string => {
    if (node.nodeType === Node.TEXT_NODE) return escapeText(node.textContent ?? "");
    if (!(node instanceof Element) || DROPPED.has(node.tagName)) return "";
    const children = Array.from(node.childNodes, clean).join("");
    const tag = TAGS[node.tagName];
    if (!tag) return children;
    if (tag === "br" || tag === "hr") return `<${tag}>`;
    let attributes = "";
    if (tag === "a") {
      const href = safeHref(node.getAttribute("href") ?? "");
      if (!href) return children;
      attributes = ` href="${escapeText(href)}"`;
    }
    const color = node.tagName === "FONT" ? node.getAttribute("color") : null;
    const style = cleanStyle(`${node.getAttribute("style") ?? ""}${color ? `;color: ${color}` : ""}`);
    if (style && STYLED.has(tag)) attributes += ` style="${escapeText(style)}"`;
    if (tag === "span" && !attributes) return children;
    return `<${tag}${attributes}>${children}</${tag}>`;
  };
  return Array.from(doc.body.childNodes, clean).join("");
}

function escapeText(text: string) {
  return text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");
}

export function textComposition(text: string) {
  return escapeText(text).replaceAll("\n", "<br>");
}

// Use the visible authored content when removing formatting, including a
// recovered draft whose cached plain alternative predates its HTML.
export function plainComposition(html: string): string {
  const doc = new DOMParser().parseFromString(cleanComposition(html), "text/html");
  const blocks = new Set(["P", "DIV", "LI", "UL", "OL", "H1", "H2", "H3", "BLOCKQUOTE", "PRE", "HR"]);
  const children = (node: Node): string => {
    let text = "";
    for (const child of node.childNodes) {
      if (child instanceof Element && blocks.has(child.tagName) && text && !text.endsWith("\n")) text += "\n";
      text += read(child);
    }
    return text;
  };
  const read = (node: Node): string => {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent ?? "";
    if (!(node instanceof Element)) return "";
    if (node.tagName === "BR") return "\n";
    if (node.tagName === "HR") return "---\n";
    const text = children(node);
    if (node.tagName === "A") {
      const href = node.getAttribute("href") ?? "";
      return href === text || href === `mailto:${text}` ? text : `${text} <${href}>`;
    }
    if (node.tagName === "BLOCKQUOTE") {
      return text.replace(/\n$/, "").split("\n").map((line) => `> ${line}`).join("\n") + "\n";
    }
    return blocks.has(node.tagName) && !["UL", "OL"].includes(node.tagName) ? text + "\n" : text;
  };
  return children(doc.body).replace(/\n$/, "");
}

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

type Shortcut = { code: string; shift?: boolean; alt?: boolean };
const keyLabel = (code: string) =>
  ({ BracketLeft: "[", BracketRight: "]", Backslash: "\\", Period: ".", Comma: "," })[code]
  ?? code.replace(/^(Key|Digit)/, "");
const describe = ({ code, shift, alt }: Shortcut) => isMac
  ? `⌘${alt ? "⌥" : ""}${shift ? "⇧" : ""}${keyLabel(code)}`
  : `Ctrl+${alt ? "Alt+" : ""}${shift ? "Shift+" : ""}${keyLabel(code)}`;
const ariaShortcut = ({ code, shift, alt }: Shortcut) =>
  `${isMac ? "Meta" : "Control"}+${alt ? "Alt+" : ""}${shift ? "Shift+" : ""}${keyLabel(code)}`;

type Action = { label: string; icon: IconName; command: string; value?: string; shortcut?: Shortcut };
// The usual word-processor shortcuts, following Google Docs and Gmail.
const HISTORY: Action[] = [
  { label: "Undo", icon: "undo", command: "undo", shortcut: { code: "KeyZ" } },
  { label: "Redo", icon: "redo", command: "redo", shortcut: { code: "KeyZ", shift: true } },
];
const INLINE: Action[] = [
  { label: "Bold", icon: "bold", command: "bold", shortcut: { code: "KeyB" } },
  { label: "Italic", icon: "italic", command: "italic", shortcut: { code: "KeyI" } },
  { label: "Underline", icon: "underline", command: "underline", shortcut: { code: "KeyU" } },
  { label: "Strikethrough", icon: "strikethrough", command: "strikeThrough", shortcut: { code: "KeyX", shift: true } },
  { label: "Superscript", icon: "superscript", command: "superscript", shortcut: { code: "Period" } },
  { label: "Subscript", icon: "subscript", command: "subscript", shortcut: { code: "Comma" } },
];
const LISTS: Action[] = [
  { label: "Bulleted list", icon: "bulletList", command: "insertUnorderedList", shortcut: { code: "Digit8", shift: true } },
  { label: "Numbered list", icon: "numberedList", command: "insertOrderedList", shortcut: { code: "Digit7", shift: true } },
  { label: "Decrease indent", icon: "outdent", command: "outdent", shortcut: { code: "BracketLeft" } },
  { label: "Increase indent", icon: "indent", command: "indent", shortcut: { code: "BracketRight" } },
];
const BLOCKS: Action[] = [
  { label: "Quote", icon: "quote", command: "formatBlock", value: "<blockquote>", shortcut: { code: "Digit9", shift: true } },
  { label: "Align left", icon: "alignLeft", command: "justifyLeft", shortcut: { code: "KeyL", shift: true } },
  { label: "Align centre", icon: "alignCenter", command: "justifyCenter", shortcut: { code: "KeyE", shift: true } },
  { label: "Align right", icon: "alignRight", command: "justifyRight", shortcut: { code: "KeyR", shift: true } },
  { label: "Horizontal line", icon: "rule", command: "insertHorizontalRule" },
];
const CLEAR: Action = { label: "Clear formatting", icon: "clearFormat", command: "removeFormat", shortcut: { code: "Backslash" } };
const TEXT_STYLES = [
  { value: "div", label: "Normal text", shortcut: { code: "Digit0", alt: true } },
  { value: "h1", label: "Heading 1", shortcut: { code: "Digit1", alt: true } },
  { value: "h2", label: "Heading 2", shortcut: { code: "Digit2", alt: true } },
  { value: "h3", label: "Heading 3", shortcut: { code: "Digit3", alt: true } },
  { value: "pre", label: "Code block" },
] as const;
const LINK_SHORTCUT: Shortcut = { code: "KeyK" };
const SHORTCUT_ACTIONS = [...INLINE, ...LISTS, ...BLOCKS, CLEAR];
const TOGGLES = [...INLINE, ...LISTS, ...BLOCKS].filter((action) => action.command !== "formatBlock" && action.command !== "insertHorizontalRule" && !action.command.endsWith("dent"));

// Mid-tones stay legible on both the light and dark composer, and on a
// recipient's white page.
const TEXT_COLOURS = [
  ["Grey", "#5f6368"], ["Red", "#d93025"], ["Orange", "#e8710a"], ["Amber", "#b06000"], ["Green", "#188038"],
  ["Teal", "#12858a"], ["Blue", "#1a73e8"], ["Purple", "#9334e6"], ["Pink", "#d01884"],
] as const;
const HIGHLIGHTS = [
  ["Yellow", "#fff2a8"], ["Green", "#ccf5d1"], ["Blue", "#d2e3fc"], ["Purple", "#e9d7fe"], ["Pink", "#fdd5e5"],
  ["Orange", "#fde2c4"],
] as const;
// Applied then removed to clear a colour without disturbing other formatting.
const SENTINEL = "#010203";
const SENTINEL_RGB = "rgb(1, 2, 3)";

type Popover = "color" | "highlight" | "link" | null;
type State = { active: ReadonlySet<string>; block: string; link: string | null; color: string; highlight: string };
const EMPTY: State = { active: new Set(), block: "div", link: null, color: "", highlight: "" };

export function CompositionBody({ body, html, disabled, onChange }: {
  body: string;
  html: string | null;
  disabled: boolean;
  onChange: (body: string, html: string | null) => void;
}) {
  const editor = useRef<HTMLDivElement>(null);
  const toolbar = useRef<HTMLDivElement>(null);
  const selection = useRef<Range | null>(null);
  const [state, setState] = useState<State>(EMPTY);
  const [popover, setPopover] = useState<Popover>(null);
  const [linkDraft, setLinkDraft] = useState({ url: "", error: false });
  const [focusIndex, setFocusIndex] = useState(0);
  useEffect(() => {
    if (editor.current) editor.current.innerHTML = cleanComposition(html ?? "");
  }, []); // The parent remounts this editor when the format/draft changes.

  const inEditor = (node: Node | null | undefined) => !!node && !!editor.current?.contains(node);
  const read = () => {
    const anchor = window.getSelection()?.anchorNode;
    const element = anchor instanceof Element ? anchor : anchor?.parentElement;
    const block = document.queryCommandValue("formatBlock").toLowerCase();
    setState({
      active: new Set([
        ...TOGGLES.filter(({ command }) => document.queryCommandState(command)).map(({ command }) => command),
        // An indented paragraph is a borderless blockquote; only real quotes count.
        ...(element?.closest('blockquote:not([style*="border: none"])') && inEditor(element) ? ["formatBlock"] : []),
      ]),
      block: TEXT_STYLES.some(({ value }) => value === block) ? block : "div",
      link: element?.closest("a")?.getAttribute("href") ?? null,
      color: document.queryCommandValue("foreColor"),
      highlight: document.queryCommandValue("backColor"),
    });
  };

  // Reflect the formatting at the caret, as editors do, while it is in the message.
  useEffect(() => {
    if (html === null) return;
    const track = () => {
      const current = window.getSelection();
      if (!current?.rangeCount || !inEditor(current.anchorNode)) return;
      selection.current = current.getRangeAt(0).cloneRange();
      read();
    };
    document.addEventListener("selectionchange", track);
    return () => document.removeEventListener("selectionchange", track);
  }, [html === null]);

  useEffect(() => {
    if (!popover) return;
    const close = (event: MouseEvent) => {
      if (!(event.target as Element).closest?.(".composition-popover-anchor")) setPopover(null);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [popover]);

  // One tab stop; arrows move between controls (WAI-ARIA toolbar pattern).
  const items = () => Array.from(toolbar.current?.querySelectorAll<HTMLElement>("[data-toolbar-item]") ?? []);
  useLayoutEffect(() => {
    items().forEach((item, index) => { item.tabIndex = index === focusIndex ? 0 : -1; });
  });

  const changed = () => {
    const node = editor.current;
    if (node) onChange(plainComposition(node.innerHTML), cleanComposition(node.innerHTML));
  };
  // Return to the caret or selection the user left in the message.
  const restore = () => {
    const node = editor.current;
    if (!node) return;
    const inside = inEditor(window.getSelection()?.anchorNode);
    node.focus();
    if (!inside && selection.current) {
      const current = window.getSelection();
      current?.removeAllRanges();
      current?.addRange(selection.current);
    }
  };
  // Native editing preserves the browser undo stack.
  const run = (command: string, value?: string) => {
    restore();
    document.execCommand(command, false, value);
    read();
    changed();
  };
  const colour = (command: "foreColor" | "backColor", value: string | null) => {
    setPopover(null);
    run(command, value ?? SENTINEL);
    if (value) return;
    const property = command === "foreColor" ? "color" : "backgroundColor";
    // Keep the coloured text selected across the clean-up, so the next
    // command (such as removing its highlight too) still applies to it.
    let first: Node | undefined;
    let last: Node | undefined;
    for (const element of editor.current?.querySelectorAll<HTMLElement>("font, span") ?? []) {
      const marked = element.getAttribute("color")?.toLowerCase() === SENTINEL || element.style[property] === SENTINEL_RGB;
      if (!marked) continue;
      if (element.getAttribute("color")?.toLowerCase() === SENTINEL) element.removeAttribute("color");
      if (element.style[property] === SENTINEL_RGB) {
        // Text colour splits its ancestors, but a background is not inherited
        // and so still shows through from an enclosing highlight.
        const outer = element.parentElement?.closest<HTMLElement>("[style*=background-color]");
        element.style[property] = outer && inEditor(outer) ? "transparent" : "";
      }
      if (!element.getAttribute("style")) element.removeAttribute("style");
      const kept = element.attributes.length ? [element] : Array.from(element.childNodes);
      if (!element.attributes.length) element.replaceWith(...kept);
      first ??= kept[0];
      last = kept.at(-1) ?? last;
    }
    if (first && last) {
      const range = document.createRange();
      range.setStartBefore(first);
      range.setEndAfter(last);
      window.getSelection()?.removeAllRanges();
      window.getSelection()?.addRange(range);
      selection.current = range.cloneRange();
    }
    read();
    changed();
  };
  const openLink = () => {
    setLinkDraft({ url: state.link ?? "", error: false });
    setPopover(popover === "link" ? null : "link");
  };
  const applyLink = () => {
    const typed = linkDraft.url.trim();
    const url = safeHref(/^[a-z][a-z0-9+.-]*:/i.test(typed) ? typed
      : /^[^\s@/]+@[^\s@/]+\.[^\s@/]+$/.test(typed) ? `mailto:${typed}` : `https://${typed}`);
    if (!typed || !url) return setLinkDraft({ ...linkDraft, error: true });
    setPopover(null);
    restore();
    const current = window.getSelection();
    const anchor = current?.anchorNode instanceof Element ? current.anchorNode : current?.anchorNode?.parentElement;
    const existing = anchor?.closest("a");
    if (current?.isCollapsed && existing && inEditor(existing)) {
      // Editing the link at the caret changes the whole link.
      const range = document.createRange();
      range.selectNodeContents(existing);
      current.removeAllRanges();
      current.addRange(range);
    }
    if (current?.isCollapsed) {
      const text = url.replace(/^mailto:/i, "");
      run("insertHTML", `<a href="${escapeText(url)}">${escapeText(text)}</a>`);
    } else {
      run("createLink", url);
    }
  };
  const removeLink = () => {
    setPopover(null);
    restore();
    const current = window.getSelection();
    const anchor = current?.anchorNode instanceof Element ? current.anchorNode : current?.anchorNode?.parentElement;
    const existing = anchor?.closest("a");
    if (current?.isCollapsed && existing && inEditor(existing)) {
      const range = document.createRange();
      range.selectNodeContents(existing);
      current.removeAllRanges();
      current.addRange(range);
    }
    run("unlink");
  };

  if (html === null) return (
    <textarea className="composer-body" aria-label="Message" value={body}
      onChange={(event) => onChange(event.target.value, null)} placeholder="Write your message" />
  );

  const button = (action: Action, pressed?: boolean) => (
    <button key={action.label} type="button" className="icon-button" data-toolbar-item
      aria-label={action.label} disabled={disabled}
      title={action.shortcut ? `${action.label} (${describe(action.shortcut)})` : action.label}
      aria-keyshortcuts={action.shortcut ? ariaShortcut(action.shortcut) : undefined}
      aria-pressed={pressed}
      // Keep the caret and selection in the message while clicking.
      onMouseDown={(event) => event.preventDefault()}
      onClick={() => {
        setPopover(null);
        if (action.command === "formatBlock" && state.active.has("formatBlock")) run("formatBlock", "<div>");
        else run(action.command, action.value);
      }}>
      <Icon name={action.icon} size={16} />
    </button>
  );
  const toggle = (action: Action) => button(action, state.active.has(action.command));
  const anchored = (name: Exclude<Popover, null>, trigger: ReactNode, panel: ReactNode) => (
    <div className="composition-popover-anchor">
      {trigger}
      {popover === name && panel}
    </div>
  );
  const swatches = (command: "foreColor" | "backColor", label: string, colours: readonly (readonly [string, string])[], none: string) => (
    <div className="composition-popover" role="dialog" aria-label={label}
      onKeyDown={(event) => {
        if (event.key === "Escape") { event.stopPropagation(); setPopover(null); restore(); }
      }}>
      <div className="composition-swatches">
        {colours.map(([name, value], index) => (
          <button key={value} type="button" className="composition-swatch" aria-label={name} title={name}
            autoFocus={index === 0} style={{ background: value }}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => colour(command, value)} />
        ))}
      </div>
      <button type="button" className="button button-quiet composition-swatch-none"
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => colour(command, null)}>{none}</button>
    </div>
  );
  const colourTrigger = (name: "color" | "highlight", label: string, icon: IconName, current: string) => (
    <button type="button" className="icon-button composition-colour" data-toolbar-item aria-label={label} title={label}
      aria-haspopup="dialog" aria-expanded={popover === name} disabled={disabled}
      onMouseDown={(event) => event.preventDefault()}
      onClick={() => setPopover(popover === name ? null : name)}>
      <Icon name={icon} size={16} />
      <span className="composition-colour-bar" style={{ background: current || undefined }} />
    </button>
  );
  const divider = (key: string) => <span key={key} className="composition-toolbar-divider" aria-hidden="true" />;

  return (
    <div className="composition-rich">
      <div ref={toolbar} className="composition-toolbar" role="toolbar" aria-label="Formatting"
        onKeyDown={(event) => {
          if ((event.target as Element).closest(".composition-popover")) return;
          const moves: Record<string, number> = { ArrowRight: 1, ArrowLeft: -1, Home: -Infinity, End: Infinity };
          if (!(event.key in moves)) return;
          event.preventDefault();
          const all = items();
          const next = Math.max(0, Math.min(all.length - 1, focusIndex + moves[event.key]!));
          setFocusIndex(next);
          all[next]?.focus();
        }}
        onFocus={(event) => {
          const index = items().indexOf(event.target as HTMLElement);
          if (index >= 0) setFocusIndex(index);
        }}>
        {HISTORY.map((action) => button(action))}
        {divider("history")}
        <select className="composition-style" data-toolbar-item aria-label="Text style" disabled={disabled}
          value={state.block} onChange={(event) => run("formatBlock", `<${event.target.value}>`)}>
          {TEXT_STYLES.map(({ value, label }) => <option key={value} value={value}>{label}</option>)}
        </select>
        {divider("style")}
        {INLINE.map(toggle)}
        {divider("inline")}
        {anchored("color", colourTrigger("color", "Text colour", "textColor", state.color),
          swatches("foreColor", "Text colour", TEXT_COLOURS, "Default colour"))}
        {anchored("highlight", colourTrigger("highlight", "Highlight", "highlight",
          /rgba\(0, 0, 0, 0\)|transparent/.test(state.highlight) ? "" : state.highlight),
          swatches("backColor", "Highlight", HIGHLIGHTS, "No highlight"))}
        {anchored("link",
          <button type="button" className="icon-button" data-toolbar-item aria-label="Link"
            title={`Link (${describe(LINK_SHORTCUT)})`} aria-keyshortcuts={ariaShortcut(LINK_SHORTCUT)}
            aria-haspopup="dialog" aria-expanded={popover === "link"} aria-pressed={!!state.link}
            disabled={disabled} onMouseDown={(event) => event.preventDefault()} onClick={openLink}>
            <Icon name="link" size={16} />
          </button>,
          <div className="composition-popover" role="dialog" aria-label="Link"
            onKeyDown={(event) => {
              if (event.key === "Escape") { event.stopPropagation(); setPopover(null); restore(); }
            }}>
          <div className="composition-link">
            <input type="text" aria-label="Link address" placeholder="Web or email address" autoFocus
              aria-invalid={linkDraft.error} value={linkDraft.url}
              onChange={(event) => setLinkDraft({ url: event.target.value, error: false })}
              // The composer is itself a form; Enter applies the link instead of submitting it.
              onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); applyLink(); } }} />
            {linkDraft.error && <p className="composition-link-error" role="alert">Enter a web or email address.</p>}
            <div className="composition-link-actions">
              {state.link && <button type="button" className="button button-quiet" onClick={removeLink}>Remove link</button>}
              <button type="button" className="button button-primary" onClick={applyLink}>Apply</button>
            </div>
          </div>
          </div>)}
        {divider("link")}
        {LISTS.map((action) => action.command.endsWith("dent") ? button(action) : toggle(action))}
        {divider("lists")}
        {BLOCKS.map((action) => action.command === "insertHorizontalRule" ? button(action) : toggle(action))}
        {divider("blocks")}
        {button(CLEAR)}
      </div>
      <div ref={editor} className="composer-body" role="textbox" aria-label="Message"
        aria-multiline="true" contentEditable={!disabled} suppressContentEditableWarning
        onKeyDown={(event) => {
          if (!(isMac ? event.metaKey : event.ctrlKey)) return;
          const matches = (shortcut?: Shortcut) => !!shortcut && shortcut.code === event.code
            && !!shortcut.shift === event.shiftKey && !!shortcut.alt === event.altKey;
          const style = TEXT_STYLES.find((candidate) => "shortcut" in candidate && matches(candidate.shortcut));
          const action = SHORTCUT_ACTIONS.find((candidate) => matches(candidate.shortcut));
          if (matches(LINK_SHORTCUT)) {
            event.preventDefault();
            openLink();
          } else if (style) {
            event.preventDefault();
            run("formatBlock", `<${style.value}>`);
          } else if (action) {
            event.preventDefault();
            if (action.command === "formatBlock" && state.active.has("formatBlock")) run("formatBlock", "<div>");
            else run(action.command, action.value);
          }
        }}
        onInput={() => { read(); changed(); }}
        onDrop={(event) => event.preventDefault()}
        onPaste={(event) => {
          event.preventDefault();
          document.execCommand("insertText", false, event.clipboardData.getData("text/plain"));
          changed();
        }} />
    </div>
  );
}
