import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { Icon } from "../components/icon";
import { PageHeading, useWorkspace } from "../components/workspace";
import {
  desktopSettings,
  errorMessage,
  isDesktop,
  type LoadedDesktopSettings,
  setDesktopSettings,
} from "../lib/native";

export const Route = createFileRoute("/settings")({ component: Appearance });

function Appearance() {
  const { theme, setTheme, density, setDensity } = useWorkspace();
  return (
    <section className="page settings-page">
      <PageHeading
        title="Appearance"
        description="A workspace that feels like yours."
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
      <DesktopSection />
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

function DesktopSection() {
  const [loaded, setLoaded] = useState<LoadedDesktopSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!isDesktop()) return;
    desktopSettings()
      .then(setLoaded)
      .catch((reason) => setError(errorMessage(reason)));
  }, []);

  if (!loaded && !error) return null;
  const settings = loaded?.settings;
  const locked = !settings || loaded?.managed || saving;

  async function choose(closeToBackground: boolean) {
    if (!settings) return;
    setSaving(true);
    setError(null);
    try {
      setLoaded(await setDesktopSettings({ ...settings, closeToBackground }));
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setSaving(false);
    }
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
              onClick={() => void choose(option.value)}
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
          Could not change desktop settings: {error}
        </div>
      )}
    </section>
  );
}
