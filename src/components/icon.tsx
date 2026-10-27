const paths = {
  plus: <path d="M12 5v14M5 12h14" />,
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
      <svg width="25" height="25" viewBox="0 0 28 28" fill="none">
        <path
          d="M6 22V7h7.4c4.4 0 6.6 2.1 6.6 5.6 0 3.6-2.5 5.8-6.8 5.8H11V22H6Z"
          fill="white"
        />
        <path
          d="M11 11v3.5h2.2c1.5 0 2.3-.6 2.3-1.8S14.7 11 13.2 11H11Z"
          fill="#748de7"
        />
        <path d="m19 5 3 3-3 3-3-3 3-3Z" fill="white" fillOpacity=".6" />
      </svg>
    </span>
  );
}
