import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { BackupSettings } from "../components/backup-settings";
import {
  useDesktopSettings,
  useSaveDesktopSettings,
} from "../components/desktop-settings";
import { Icon } from "../components/icon";
import { useRefreshSend, useSendState } from "../components/send-state";
import { PageHeading, useWorkspace } from "../components/workspace";
import {
  type DesktopSettings,
  distrustSender,
  errorMessage,
  isDesktop,
  setUndoSendSeconds,
  trustedSenders,
} from "../lib/native";

export const Route = createFileRoute("/settings")({ component: Settings });

function Settings() {
  const { theme, setTheme, density, setDensity } = useWorkspace();
  return (
    <section className="page settings-page">
      <PageHeading
        title="Settings"
        description="A workspace that feels like yours, and a way back if this device fails."
      />
      <section className="settings-section">
        <h2>Colour theme</h2>
        <p>Follow your desktop, or choose a look of your own.</p>
        <div className="theme-options" role="group" aria-label="Colour theme">
          {(
            [
              { value: "light", label: "Light", icon: "sun" },
              { value: "dark", label: "Dark", icon: "moon" },
              { value: "system", label: "System", icon: "monitor" },
            ] as const
          ).map((option) => (
            <button
              type="button"
              aria-pressed={theme === option.value}
              key={option.value}
              onClick={() => setTheme(option.value)}
              className={`theme-option theme-${option.value}`}
            >
              <span className="theme-preview" aria-hidden="true">
                <span className="mini-sidebar">
                  <i />
                  <i />
                  <i />
                </span>
                <span className="mini-content">
                  <i />
                  <i />
                  <i />
                </span>
              </span>
              <span className="theme-option-label">
                <Icon name={option.icon} size={16} />
                {option.label}
                <span className="theme-selected">
                  {theme === option.value && <Icon name="check" size={13} />}
                </span>
              </span>
            </button>
          ))}
        </div>
      </section>
      <section className="settings-section density-section">
        <div>
          <h2>Interface density</h2>
          <p>A little breathing room, or more in view.</p>
        </div>
        <div
          className="segmented-control"
          role="group"
          aria-label="Interface density"
        >
          {(["comfortable", "compact"] as const).map((value) => (
            <button
              key={value}
              type="button"
              aria-pressed={density === value}
              onClick={() => setDensity(value)}
            >
              {value === "comfortable" ? "Comfortable" : "Compact"}
            </button>
          ))}
        </div>
      </section>
      <section className="settings-section density-section">
        <div>
          <h2>Signatures</h2>
          <p>Choose the text added to messages from each account.</p>
        </div>
        <Link to="/settings/signatures" className="button button-secondary">
          Manage signatures
        </Link>
      </section>
      <BackupSettings />
      <UndoDelay />
      <DesktopSection />
      <TrustedSenders />
      <div className="settings-note">
        <Icon name="monitor" size={16} />
        <p>
          Preferences are saved on this device. System theme follows your
          desktop’s light or dark appearance.
        </p>
      </div>
    </section>
  );
}

const UNDO_CHOICES = [0, 5, 10, 20, 30] as const;

function UndoDelay() {
  const send = useSendState();
  const refresh = useRefreshSend();
  const change = useMutation({
    mutationFn: setUndoSendSeconds,
    onSuccess: refresh,
  });
  if (!send.data) return null;
  const current = change.isPending
    ? change.variables
    : send.data.undo_send_seconds;
  return (
    <section className="settings-section density-section">
      <div>
        <h2>Undo send</h2>
        <p>
          How long Pigeon waits before sending, so you can take a message back.
        </p>
        {change.isError && (
          <p role="alert" className="mail-inline-error">
            {errorMessage(change.error)}
          </p>
        )}
      </div>
      <div className="segmented-control" role="group" aria-label="Undo send">
        {UNDO_CHOICES.map((seconds) => (
          <button
            key={seconds}
            type="button"
            aria-pressed={current === seconds}
            disabled={change.isPending}
            onClick={() => change.mutate(seconds)}
          >
            {seconds === 0 ? "Off" : `${seconds}s`}
          </button>
        ))}
      </div>
    </section>
  );
}

function DesktopSection() {
  const { data: loaded, error: loadError } = useDesktopSettings();
  const save = useSaveDesktopSettings();
  const error = loadError ?? save.error;

  if (!loaded && !error) return null;
  const settings = loaded?.settings;
  const locked = !settings || loaded?.managed || save.isPending;

  function choose(change: Partial<DesktopSettings>) {
    if (settings) save.mutate({ ...settings, ...change });
  }

  return (
    <section className="settings-section" aria-labelledby="desktop-heading">
      <div className="density-section">
        <div>
          <h2 id="desktop-heading">When the window closes</h2>
          <p>
            Keep Pigeon syncing in the background and reopen it from your
            launcher, or quit completely. Quit Pigeon in the sidebar always
            exits.
          </p>
        </div>
        <div
          className="segmented-control"
          role="group"
          aria-label="When the window closes"
        >
          {(
            [
              { value: true, label: "Keep running" },
              { value: false, label: "Quit" },
            ] as const
          ).map((option) => (
            <button
              key={option.label}
              type="button"
              aria-pressed={settings?.closeToBackground === option.value}
              disabled={locked}
              onClick={() => choose({ closeToBackground: option.value })}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>
      <div className="density-section">
        <div>
          <h2>Sender images</h2>
          <p>
            Show each sender’s initials, or look up their website’s icon. Lookups
            tell DuckDuckGo which domains email you, never full addresses, and
            icons stay in your encrypted mail store. An icon does not prove who
            sent a message.
          </p>
        </div>
        <div
          className="segmented-control"
          role="group"
          aria-label="Sender images"
        >
          {(
            [
              { value: false, label: "Initials" },
              { value: true, label: "Website icons" },
            ] as const
          ).map((option) => (
            <button
              key={option.label}
              type="button"
              aria-pressed={settings?.senderImages === option.value}
              disabled={locked}
              onClick={() => choose({ senderImages: option.value })}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>
      <div className="density-section" id="remote-images">
        <div>
          <h2>Remote images</h2>
          <p>
            Images a message loads from the web are blocked until you ask for
            them, because loading one tells the sender’s server that you opened
            the message, when, and your IP address. Embedded images always show.
          </p>
        </div>
        <div
          className="segmented-control"
          role="group"
          aria-label="Remote images"
        >
          {(
            [
              { value: false, label: "Ask first" },
              { value: true, label: "Always load" },
            ] as const
          ).map((option) => (
            <button
              key={option.label}
              type="button"
              aria-pressed={settings?.remoteImages === option.value}
              disabled={locked}
              onClick={() => choose({ remoteImages: option.value })}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>
      {loaded?.managed && (
        <p>
          Managed by Home Manager. Change <code>programs.pigeon.settings</code>{" "}
          in your Nix configuration.
        </p>
      )}
      {loaded?.warning && (
        <p role="status">{loaded.warning}</p>
      )}
      {error && (
        <div className="notice notice-error" role="alert">
          Could not change desktop settings: {errorMessage(error)}
        </div>
      )}
    </section>
  );
}

const TRUSTED_SENDERS_KEY = ["mail", "trusted-senders"] as const;

function TrustedSenders() {
  const client = useQueryClient();
  const list = useQuery({
    queryKey: TRUSTED_SENDERS_KEY,
    queryFn: trustedSenders,
    enabled: isDesktop(),
  });
  const remove = useMutation({
    mutationFn: ({ email, host }: { email: string; host?: string }) =>
      distrustSender(email, host),
    onSuccess: () => client.invalidateQueries({ queryKey: TRUSTED_SENDERS_KEY }),
  });
  // Like the other mail settings, hidden while the mail store is unavailable.
  if (!list.data) return null;
  return (
    <section className="settings-section" aria-labelledby="trusted-heading">
      <h2 id="trusted-heading">Trusted senders</h2>
      <p>
        Remote images from these addresses load without asking, from every
        source or only the ones you chose. Add one from a message’s formatted
        view. A sender’s address can be forged, so only trust senders whose
        images you would load anyway.
      </p>
      {list.data.length ? (
        <ul className="trusted-senders">
          {list.data.map((sender) => (
            <li key={sender.email}>
              <div>
                <span>{sender.email}</span>
                {sender.all ? (
                  <small>All sources</small>
                ) : (
                  <ul className="trusted-hosts" aria-label={`Sources trusted for ${sender.email}`}>
                    {sender.hosts.map((host) => (
                      <li key={host}>
                        {host}
                        <button
                          type="button"
                          disabled={remove.isPending}
                          onClick={() => remove.mutate({ email: sender.email, host })}
                          aria-label={`Stop loading images from ${host} for ${sender.email}`}
                        >
                          ×
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
              <button
                type="button"
                className="button button-quiet"
                disabled={remove.isPending}
                onClick={() => remove.mutate({ email: sender.email })}
                aria-label={`Stop loading images from ${sender.email}`}
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p>No trusted senders yet.</p>
      )}
      {remove.isError && (
        <p role="alert" className="mail-inline-error">
          {errorMessage(remove.error)}
        </p>
      )}
    </section>
  );
}
