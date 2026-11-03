const paths = {
  plus: <path d="M12 5v14M5 12h14" />,
  send: <path d="M21 3 10 14M21 3l-7 18-4-7-7-4Z" />,
  edit: <><path d="M4 20h4L19 9l-4-4L4 16Z" /><path d="m13.5 6.5 4 4" /></>,
  outbox: <><path d="M4 13v6h16v-6" /><path d="M12 15V4m-4 4 4-4 4 4" /></>,
  close: <path d="m6 6 12 12M6 18 18 6" />,
  inbox: <><path d="m4 5-2 9v5h20v-5l-2-9Z" /><path d="M2 14h6l2 3h4l2-3h6" /></>,
  archive: <><rect x="3" y="3" width="18" height="5" rx="1.5" /><path d="M5 8v12h14V8M10 12h4" /></>,
  trash: <><path d="M4 6h16M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7" /></>,
  file: <><path d="M14 3H5v18h14V8Zm0 0v5h5M8 12h8m-8 4h6" /></>,
  grid: (
    <>
      <rect x="3.5" y="3.5" width="6" height="6" rx="1.4" />
      <rect x="14.5" y="3.5" width="6" height="6" rx="1.4" />
      <rect x="3.5" y="14.5" width="6" height="6" rx="1.4" />
      <rect x="14.5" y="14.5" width="6" height="6" rx="1.4" />
    </>
  ),
  mail: (
    <>
      <rect x="3" y="5" width="18" height="14" rx="3" />
      <path d="m4 7 8 6 8-6" />
    </>
  ),
  search: (
    <>
      <circle cx="10.5" cy="10.5" r="6.5" />
      <path d="m16 16 4.5 4.5" />
    </>
  ),
  settings: (
    <>
      <path d="M4 7h16M4 17h16" />
      <circle cx="9" cy="7" r="2.5" />
      <circle cx="15" cy="17" r="2.5" />
    </>
  ),
  shield: (
    <>
      <path d="m12 3 8 3v6c0 4.5-4.8 7.8-8 9-3.2-1.2-8-4.5-8-9V6Z" />
      <path d="m8.5 12 2.5 2.5 4.5-5" />
    </>
  ),
  lock: (
    <>
      <rect x="5" y="10" width="14" height="11" rx="2.5" />
      <path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v3" />
    </>
  ),
  monitor: (
    <>
      <rect x="3" y="4" width="18" height="13" rx="2.5" />
      <path d="M8 21h8m-4-4v4" />
    </>
  ),
  chevron: <path d="m9 6 6 6-6 6" />,
  check: <path d="m5 12 4 4L19 6" />,
  bold: <path d="M7 5h6a3.5 3.5 0 0 1 0 7H7Zm0 7h7a3.5 3.5 0 0 1 0 7H7Z" strokeWidth="2.2" />,
  italic: <path d="M11 5h7M6 19h7m1-14-4 14" />,
  underline: <path d="M7 4v7a5 5 0 0 0 10 0V4M5 20h14" />,
  strikethrough: <path d="M16.5 7.5C16 5.8 14.3 5 12 5c-2.6 0-4.3 1.3-4.3 3.2 0 1.3.8 2.2 2.3 2.8M4 12h16m-3.5 2.5c.2.4.3.9.3 1.4 0 2-2 3.1-4.8 3.1-2.6 0-4.5-1-5-2.8" />,
  superscript: <><path d="m4 8 8 11m0-11-8 11" /><path d="M15.5 6c.2-1.2 3.2-1.6 3.4 0 .2 1.2-1.4 2-3.4 3.8h3.8" strokeWidth="1.3" /></>,
  subscript: <><path d="m4 5 8 11m0-11-8 11" /><path d="M15.5 16c.2-1.2 3.2-1.6 3.4 0 .2 1.2-1.4 2-3.4 3.8h3.8" strokeWidth="1.3" /></>,
  textColor: <path d="m6.5 16 5.5-12 5.5 12M8.6 11.5h6.8" />,
  highlight: <><path d="m15 3.5 5.5 5.5-8.5 8.5H6.5V12Z" /><path d="m12 6.5 5.5 5.5" /></>,
  link: <><path d="M10 14a4 4 0 0 0 5.7 0l3-3A4 4 0 0 0 13 5.3l-1 1" /><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" /></>,
  undo: <><path d="M9 14 4 9l5-5" /><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11" /></>,
  redo: <><path d="m15 14 5-5-5-5" /><path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13" /></>,
  outdent: <><path d="M4 5h16M11 10h9M11 14h9M4 19h16" /><path d="m8 9-3 3 3 3" /></>,
  indent: <><path d="M4 5h16M11 10h9M11 14h9M4 19h16" /><path d="m5 9 3 3-3 3" /></>,
  quote: <path d="M5 11h4v6H5v-6c0-3 1.5-5 4-6m6 6h4v6h-4v-6c0-3 1.5-5 4-6" />,
  alignLeft: <path d="M4 6h16M4 10h10M4 14h16M4 18h10" />,
  alignCenter: <path d="M4 6h16M7 10h10M4 14h16M7 18h10" />,
  alignRight: <path d="M4 6h16M10 10h10M4 14h16M10 18h10" />,
  rule: <><path d="M3 12h18" /><path d="M7 6.5h10M7 17.5h10" opacity=".4" /></>,
  clearFormat: <path d="M5 5h12m-5 0-3.5 14M14 14l6 6m0-6-6 6" />,
  bulletList: (
    <>
      <path d="M10 6h10M10 12h10M10 18h10" />
      <path d="M5 6h.01M5 12h.01M5 18h.01" strokeWidth="2.6" />
    </>
  ),
  numberedList: (
    <>
      <path d="M11 6h9M11 12h9M11 18h9" />
      <path d="M4 4.5 5.5 4v4.5M4 8.5h3M4 11.2c.4-.7 2.8-.8 2.8.6 0 1-2.8 1.8-2.8 3h3M4 17h2.5L5 18.5c1.2 0 1.9.4 1.9 1.2 0 1-1.7 1.3-2.9.6" strokeWidth="1.3" />
    </>
  ),
  checkCircle: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="m8 12 3 3 5-6" />
    </>
  ),
  alert: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v6m0 4h.01" />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v6m0-10h.01" />
    </>
  ),
  refresh: (
    <>
      <path d="M20 7v5h-5M4 17v-5h5" />
      <path d="M6 7a7 7 0 0 1 12-1l2 3M4 15l2 3a7 7 0 0 0 12-1" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5" />
    </>
  ),
  moon: <path d="M20.5 14.5A9 9 0 0 1 9.5 3.5a9 9 0 1 0 11 11Z" />,
  keyboard: (
    <>
      <rect x="2.5" y="5" width="19" height="14" rx="3" />
      <path d="M6 9h.01M10 9h.01M14 9h.01M18 9h.01M6 12h.01M10 12h.01M14 12h.01M18 12h.01M7 15h10" />
    </>
  ),
  power: (
    <>
      <path d="M12 3v9M6 5.5a9 9 0 1 0 12 0" />
    </>
  ),
} as const;

export type IconName = keyof typeof paths;

export function Icon({
  name,
  size = 18,
  className,
}: {
  name: IconName;
  size?: number;
  className?: string;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.65"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className={className}
    >
      {paths[name]}
    </svg>
  );
}

export function AppMark({ small = false }: { small?: boolean }) {
  return (
    <span
      className={`app-mark${small ? " app-mark-small" : ""}`}
      aria-hidden="true"
    >
      {/* The pigeon from assets/icon.svg, without the soft neck sheen. */}
      <svg width="100%" height="100%" viewBox="0 0 512 512" fill="none">
        <path
          d="M164 520C164 430 150 350 168 270C184 172 234 106 310 106C372 106 404 150 408 192C412 240 374 270 354 310C332 360 342 450 368 520Z"
          fill="white"
        />
        <path d="M402 178Q448 192 478 224Q446 234 404 230Z" fill="#3b4580" />
        <circle cx="322" cy="184" r="30" fill="#f59e0b" />
        <circle cx="322" cy="184" r="16" fill="#1e1b2e" />
      </svg>
    </span>
  );
}
