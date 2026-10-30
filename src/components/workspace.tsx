import { Link, useNavigate, useRouterState } from "@tanstack/react-router";
import {
  createContext,
  useContext,
  useEffect,
  useId,
  useState,
  type ReactNode,
} from "react";
import { isDesktop, quitApp } from "../lib/native";
import { AppMark, Icon, type IconName } from "./icon";
import { Modal } from "./modal";
import { MailSidebar } from "./mail-sidebar";
import { useMail } from "./mail-provider";

type Theme = "system" | "light" | "dark";
type Density = "comfortable" | "compact";
type WorkspaceState = {
  theme: Theme;
  density: Density;
  setTheme: (theme: Theme) => void;
  setDensity: (density: Density) => void;
  openCommands: () => void;
  openAbout: () => void;
  shortcut: string;
};
const WorkspaceContext = createContext<WorkspaceState | null>(null);

export function useWorkspace() {
  const value = useContext(WorkspaceContext);
  if (!value) throw new Error("Workspace provider is missing");
  return value;
}

function readPreference(key: string) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function writePreference(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* Session-only preferences when browser storage is restricted. */
  }
}

export function Workspace({ children }: { children: ReactNode }) {
  const mail = useMail();
  const navigate = useNavigate();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const [theme, changeTheme] = useState<Theme>("system");
  const [density, changeDensity] = useState<Density>("comfortable");
  const [commandsOpen, setCommandsOpen] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [shortcut, setShortcut] = useState("Ctrl");
  const [desktop, setDesktop] = useState(false);
  const [ready, setReady] = useState(false);
  const [quitError, setQuitError] = useState<string | null>(null);

  useEffect(() => {
    const storedTheme = readPreference("pigeon.theme");
    const storedDensity = readPreference("pigeon.density");
    if (
      storedTheme === "light" ||
      storedTheme === "dark" ||
      storedTheme === "system"
    )
      changeTheme(storedTheme);
    if (storedDensity === "compact" || storedDensity === "comfortable")
      changeDensity(storedDensity);
    setShortcut(/Mac|iPhone|iPad/.test(navigator.platform) ? "⌘" : "Ctrl");
    setDesktop(isDesktop());
    setReady(true);
  }, []);
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);
  useEffect(() => {
    document.documentElement.dataset.density = density;
  }, [density]);
  useEffect(() => {
    function keydown(event: KeyboardEvent) {
      if (
        (event.metaKey || event.ctrlKey) &&
        !event.altKey &&
        event.key.toLowerCase() === "k"
      ) {
        event.preventDefault();
        const dialog = document.querySelector("dialog[open]");
        if (
          !aboutOpen &&
          (!dialog || dialog.classList.contains("command-dialog"))
        )
          setCommandsOpen((open) => !open);
      }
    }
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, [aboutOpen]);

  const openAbout = () => {
    setCommandsOpen(false);
    setAboutOpen(true);
  };
  const currentPage =
    pathname === "/foundation"
      ? "Diagnostics"
      : pathname === "/settings"
        ? "Appearance"
        : pathname === "/mail"
          ? "Mail"
          : pathname === "/pending"
            ? "Pending changes"
            : "Overview";

  return (
    <WorkspaceContext.Provider
      value={{
        theme,
        density,
        setTheme: (value) => {
          changeTheme(value);
          writePreference("pigeon.theme", value);
        },
        setDensity: (value) => {
          changeDensity(value);
          writePreference("pigeon.density", value);
        },
        openCommands: () => setCommandsOpen(true),
        openAbout,
        shortcut,
      }}
    >
      <div className="app-shell" inert={!ready}>
        <aside className="sidebar" aria-label="Workspace sidebar">
          <Link
            to="/"
            className="brand"
            aria-label="Pigeon home"
            activeOptions={{ exact: true }}
          >
            <AppMark />
            <span className="brand-word">
              Pigeon<span>Your own space.</span>
            </span>
          </Link>
          <button
            className="sidebar-search"
            type="button"
            onClick={() => setCommandsOpen(true)}
            aria-label="Open command menu"
          >
            <Icon name="search" />
            <span className="sidebar-label">Jump to…</span>
            <kbd>{shortcut} K</kbd>
          </button>
          <nav aria-label="Main navigation" className="main-navigation">
            <span className="section-label">Workspace</span>
            <Link
              to="/"
              activeOptions={{ exact: true }}
              className="nav-item"
              title="Overview"
            >
              <Icon name="grid" />
              <span className="sidebar-label">Overview</span>
              <span className="nav-active-dot" />
            </Link>
            <Link
              to="/mail"
              search={{ view: "inbox" }}
              activeOptions={{ exact: true }}
              className="nav-item"
              title="All inboxes"
            >
              <Icon name="inbox" />
              <span className="sidebar-label">All inboxes</span>
            </Link>
            <Link
              to="/mail"
              search={{ view: "all" }}
              activeOptions={{ exact: true }}
              className="nav-item"
              title="All mail"
            >
              <Icon name="mail" />
              <span className="sidebar-label">All mail</span>
            </Link>
          </nav>
          <MailSidebar />
          <div className="sidebar-bottom">
            <nav aria-label="Preferences" className="secondary-navigation">
              <Link to="/settings" className="nav-item" title="Appearance">
                <Icon name="settings" />
                <span className="sidebar-label">Appearance</span>
              </Link>
              <Link to="/foundation" className="nav-item" title="Diagnostics">
                <Icon name="shield" />
                <span className="sidebar-label">Diagnostics</span>
              </Link>
              <button
                type="button"
                className="nav-item"
                title="About Pigeon"
                onClick={openAbout}
              >
                <Icon name="info" />
                <span className="sidebar-label">About Pigeon</span>
              </button>
              {desktop && (
                <button
                  type="button"
                  className="nav-item"
                  title="Quit Pigeon"
                  onClick={() => {
                    setQuitError(null);
                    void quitApp().catch((error) =>
                      setQuitError(String(error)),
                    );
                  }}
                >
                  <Icon name="power" />
                  <span className="sidebar-label">Quit Pigeon</span>
                </button>
              )}
            </nav>
            <div className="sidebar-build">
              <span className="build-dot" />
              <span className="sidebar-label">Mail preview</span>
              <span className="build-version sidebar-label">0.1</span>
            </div>
          </div>
        </aside>
        <div className="workspace">
          <header className="toolbar">
            <div className="breadcrumb">
              <span>Workspace</span>
              <Icon name="chevron" size={12} />
              <strong>{currentPage}</strong>
            </div>
            <div className="toolbar-actions">
              <button
                type="button"
                className="icon-button"
                title="Open command menu"
                aria-label="Search commands"
                onClick={() => setCommandsOpen(true)}
              >
                <Icon name="search" />
              </button>
              <span className="toolbar-divider" />
              <button
                type="button"
                className="workspace-avatar"
                aria-label="About this local workspace"
                onClick={openAbout}
              >
                <Icon name="monitor" size={16} />
              </button>
            </div>
          </header>
          <main id="main-content" className="page-area">
            {quitError && (
              <div className="notice notice-error" role="alert">
                Could not quit Pigeon: {quitError}
              </div>
            )}
            {mail.actionError && (
              <div className="mail-action-error" role="alert">
                <Icon name="alert" size={15} />
                <span>{mail.actionError}</span>
                <button type="button" onClick={mail.clearActionError}>
                  Dismiss
                </button>
              </div>
            )}
            {children}
          </main>
          <footer className="workspace-footer">
            <span>
              <Icon name="lock" size={12} />
              Local workspace
            </span>
            <button type="button" onClick={() => setCommandsOpen(true)}>
              <span>Quick navigation</span>
              <kbd>{shortcut} K</kbd>
            </button>
          </footer>
        </div>
      </div>
      <CommandMenu
        open={commandsOpen}
        onDismiss={() => setCommandsOpen(false)}
        onAbout={openAbout}
      />
      <Modal
        open={aboutOpen}
        onDismiss={() => setAboutOpen(false)}
        title="A better home for your mail"
        className="about-dialog"
      >
        <AppMark />
        <span className="preview-pill">Read-only mail preview</span>
        <h2>A better home for your mail</h2>
        <p>
          Pigeon brings your IMAP accounts into one private, local workspace.
          Connect accounts, read cached messages offline, and search your
          encrypted mail store.
        </p>
        <div className="about-note">
          <Icon name="info" />
          <p>
            This build receives and searches mail. Sending, mailbox
            organisation, attachment downloads, and embedded-image handling are
            coming next.
          </p>
        </div>
        <div className="modal-actions">
          <button
            type="button"
            className="button button-quiet"
            onClick={() => setAboutOpen(false)}
          >
            Got it
          </button>
          <button
            type="button"
            className="button button-primary"
            onClick={() => {
              setAboutOpen(false);
              void navigate({ to: "/foundation" });
            }}
          >
            Open diagnostics
          </button>
        </div>
      </Modal>
    </WorkspaceContext.Provider>
  );
}

export function PageHeading({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children?: ReactNode;
}) {
  return (
    <div className="page-heading">
      <div>
        <h1>{title}</h1>
        <p>{description}</p>
      </div>
      {children}
    </div>
  );
}

function CommandMenu({
  open,
  onDismiss,
  onAbout,
}: {
  open: boolean;
  onDismiss: () => void;
  onAbout: () => void;
}) {
  const navigate = useNavigate();
  const listId = useId();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
    }
  }, [open]);
  const commands: {
    label: string;
    detail: string;
    icon: IconName;
    run: () => void;
  }[] = [
    {
      label: "Search mail",
      detail: "Search your local mail",
      icon: "search",
      run: () => {
        void navigate({ to: "/mail", search: {} });
      },
    },
    {
      label: "Overview",
      detail: "All your accounts",
      icon: "grid",
      run: () => {
        void navigate({ to: "/" });
      },
    },
    {
      label: "Appearance",
      detail: "Theme and density",
      icon: "settings",
      run: () => {
        void navigate({ to: "/settings" });
      },
    },
    {
      label: "Diagnostics",
      detail: "Storage and security checks",
      icon: "shield",
      run: () => {
        void navigate({ to: "/foundation" });
      },
    },
    {
      label: "About Pigeon",
      detail: "About this preview",
      icon: "info",
      run: onAbout,
    },
  ];
  const filtered = commands.filter((command) =>
    `${command.label} ${command.detail}`
      .toLowerCase()
      .includes(query.toLowerCase().trim()),
  );
  function select(index: number) {
    const command = filtered[index];
    if (command) {
      onDismiss();
      command.run();
    }
  }
  return (
    <Modal
      open={open}
      onDismiss={onDismiss}
      title="Command menu"
      className="command-dialog"
    >
      <div className="command-input">
        <Icon name="search" size={21} />
        <input
          aria-label="Search commands"
          placeholder="Where would you like to go?"
          role="combobox"
          aria-controls={listId}
          aria-expanded={open}
          aria-autocomplete="list"
          aria-activedescendant={
            filtered[active] ? `${listId}-${active}` : undefined
          }
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setActive(0);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              setActive((index) =>
                filtered.length
                  ? (index +
                      (event.key === "ArrowDown" ? 1 : -1) +
                      filtered.length) %
                    filtered.length
                  : 0,
              );
            } else if (event.key === "Enter") {
              event.preventDefault();
              select(active);
            }
          }}
        />
        <kbd>esc</kbd>
      </div>
      <div
        className="command-results"
        role="listbox"
        id={listId}
        aria-label="Commands"
      >
        {filtered.length ? (
          filtered.map((command, index) => (
            <button
              type="button"
              role="option"
              aria-selected={index === active}
              id={`${listId}-${index}`}
              key={command.label}
              onClick={() => select(index)}
              onMouseMove={() => setActive(index)}
            >
              <span className="command-icon">
                <Icon name={command.icon} />
              </span>
              <span>
                {command.label}
                <small>{command.detail}</small>
              </span>
              {index === active && <span className="command-enter">↵</span>}
            </button>
          ))
        ) : (
          <p className="command-empty">
            No commands found. Try “appearance” or “diagnostics”.
          </p>
        )}
      </div>
      <div className="command-footer">
        <AppMark small />
        <span>Pigeon</span>
        <span className="command-hints">
          <kbd>↑</kbd>
          <kbd>↓</kbd> Navigate <kbd>↵</kbd> Open
        </span>
      </div>
    </Modal>
  );
}
