import { createFileRoute, Link } from "@tanstack/react-router";
import { Icon } from "../components/icon";
import { PageHeading, useWorkspace } from "../components/workspace";

export const Route = createFileRoute("/")({ component: Overview });

function Overview() {
  const { openCommands, openAbout, shortcut, theme } = useWorkspace();
  return (
    <section className="page overview-page">
      <PageHeading
        title="Overview"
        description="All your accounts. One clear view."
      >
        <span className="account-count">
          <span />
          No accounts connected
        </span>
      </PageHeading>
      <div className="overview-grid">
        <div className="welcome-panel">
          <div className="mail-illustration" aria-hidden="true">
            <div className="letter letter-back" />
            <div className="letter letter-front">
              <span />
              <span />
              <span />
            </div>
            <div className="envelope">
              <Icon name="mail" size={48} />
            </div>
            <span className="illustration-spark spark-one" />
            <span className="illustration-spark spark-two" />
          </div>
          <h2>Your inbox starts here.</h2>
          <p>
            A little less noise. A clearer view of your mail.
            <br />
            Your accounts, together in your own space.
          </p>
          <button
            className="button button-primary"
            type="button"
            onClick={openAbout}
          >
            Explore this preview
            <Icon name="chevron" size={15} />
          </button>
          <span className="welcome-footnote">
            Account connections are coming next.
          </span>
        </div>
        <aside className="device-panel" aria-label="Local workspace details">
          <div className="panel-heading">
            <span className="surface-icon">
              <Icon name="monitor" size={20} />
            </span>
            <h2>On this device</h2>
          </div>
          <p className="panel-description">A private space for your mail.</p>
          <dl className="device-details">
            <div>
              <dt>Mail accounts</dt>
              <dd>None connected</dd>
            </div>
            <div>
              <dt>Encrypted storage</dt>
              <dd>
                <Link to="/foundation">
                  Check status
                  <Icon name="chevron" size={12} />
                </Link>
              </dd>
            </div>
            <div>
              <dt>Mail sync</dt>
              <dd>Coming next</dd>
            </div>
          </dl>
          <div className="privacy-note">
            <Icon name="lock" size={16} />
            <p>
              Direct to your mail servers.
              <br />
              No Pigeon cloud in between.
            </p>
          </div>
        </aside>
      </div>
      <section className="quick-actions" aria-labelledby="make-it-yours">
        <div className="section-heading">
          <h2 id="make-it-yours">Make yourself at home</h2>
          <span>A few useful places to start</span>
        </div>
        <div className="action-group">
          <Link to="/settings" className="action-row">
            <span className="action-icon appearance-icon">
              <Icon name="settings" size={19} />
            </span>
            <span className="action-copy">
              <strong>Make it yours</strong>
              <small>Appearance, theme, and density</small>
            </span>
            <span className="action-meta">
              {theme[0]?.toUpperCase()}
              {theme.slice(1)}
            </span>
            <Icon name="chevron" size={15} />
          </Link>
          <button type="button" className="action-row" onClick={openCommands}>
            <span className="action-icon">
              <Icon name="keyboard" size={19} />
            </span>
            <span className="action-copy">
              <strong>Find your way faster</strong>
              <small>Jump anywhere with the command menu</small>
            </span>
            <kbd>{shortcut} K</kbd>
            <Icon name="chevron" size={15} />
          </button>
          <Link to="/foundation" className="action-row">
            <span className="action-icon">
              <Icon name="shield" size={19} />
            </span>
            <span className="action-copy">
              <strong>Check your workspace</strong>
              <small>Local storage, keyring, and viewer isolation</small>
            </span>
            <span className="action-meta">Diagnostics</span>
            <Icon name="chevron" size={15} />
          </Link>
        </div>
      </section>
      <div className="overview-note">
        <Icon name="info" size={14} />
        <p>
          You’re using an early foundation build.{" "}
          <button type="button" onClick={openAbout}>
            What’s available?
          </button>
        </p>
      </div>
    </section>
  );
}
