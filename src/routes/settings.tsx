import { createFileRoute } from "@tanstack/react-router";
import { BackupSettings } from "../components/backup-settings";
import { Icon } from "../components/icon";
import { PageHeading, useWorkspace } from "../components/workspace";

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
      <BackupSettings />
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
