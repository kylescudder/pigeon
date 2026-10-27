import { createFileRoute, Link } from "@tanstack/react-router";
import { Icon } from "../components/icon";
import { PageHeading, useWorkspace } from "../components/workspace";
import { MailGate, useMail } from "../components/mail-provider";
import { ManageAccount } from "../components/manage-account";
import { useState } from "react";
import type { MailAccount } from "../lib/native";

export const Route = createFileRoute("/")({ component: Overview });

function Overview() {
  const { openCommands, openAbout, shortcut, theme } = useWorkspace();
  const mail = useMail();
  const [managed, setManaged] = useState<MailAccount | null>(null);
  if (mail.desktop && (mail.loading || mail.error))
    return (
      <MailGate>
        <span />
      </MailGate>
    );
  if (mail.state?.accounts.length) {
    const folders = mail.state.folders;
    const cached = folders.reduce((sum, folder) => sum + folder.cached, 0);
    const unread = folders
      .filter((folder) => folder.kind === "inbox")
      .reduce((sum, folder) => sum + folder.unread, 0);
    return (
      <section className="page overview-page connected-overview">
        <PageHeading
          title="Overview"
          description="All your accounts. One clear view."
        >
          <button
            type="button"
            className="button button-primary"
            onClick={mail.connect}
          >
            <Icon name="plus" size={15} />
            Connect another account
          </button>
        </PageHeading>
        <div className="estate-summary">
          <div>
            <span>Connected accounts</span>
            <strong>{mail.state.accounts.length}</strong>
          </div>
          <div>
            <span>Unread in inboxes</span>
            <strong>{unread.toLocaleString()}</strong>
          </div>
          <div>
            <span>Cached messages</span>
            <strong>{cached.toLocaleString()}</strong>
          </div>
          <Link
            to="/mail"
            search={{ view: "inbox" }}
            className="button button-secondary"
          >
            Open all inboxes
            <Icon name="chevron" size={14} />
          </Link>
        </div>
        <div className="section-heading">
          <h2>Your accounts</h2>
          <span>Read and search downloaded mail offline</span>
        </div>
        <div className="account-cards">
          {mail.state.accounts.map((account, index) => {
            const accountFolders = folders.filter(
              (folder) => folder.account_id === account.id,
            );
            const cached = accountFolders.reduce(
              (sum, folder) => sum + folder.cached,
              0,
            );
            const unread = accountFolders
              .filter((folder) => folder.kind === "inbox")
              .reduce((sum, folder) => sum + folder.unread, 0);
            const syncing =
              account.status === "syncing" || mail.active.has(account.id);
            return (
              <article className="account-card" key={account.id}>
                <div className="account-card-heading">
                  <span className={`account-avatar avatar-${index % 4}`}>
                    {account.config.name.slice(0, 1).toUpperCase()}
                  </span>
                  <div>
                    <h3>{account.config.name}</h3>
                    <p>{account.config.address}</p>
                  </div>
                  <button
                    type="button"
                    className="icon-button"
                    aria-label={`Manage ${account.config.name}`}
                    title="Manage account"
                    onClick={() => setManaged(account)}
                  >
                    <Icon name="settings" size={16} />
                  </button>
                </div>
                <div className="account-card-counts">
                  <span>
                    <strong>{unread.toLocaleString()}</strong> unread
                  </span>
                  <span>
                    <strong>{cached.toLocaleString()}</strong> cached
                  </span>
                  <span className="connection-badge">
                    <Icon name="lock" size={11} />
                    {account.config.security === "tls" ? "TLS" : "STARTTLS"}
                  </span>
                </div>
                <div
                  className={`account-sync-status${account.error ? " sync-status-error" : ""}`}
                >
                  <Icon
                    name={
                      syncing
                        ? "refresh"
                        : account.error
                          ? "alert"
                          : "checkCircle"
                    }
                    size={14}
                    className={syncing ? "spinning" : undefined}
                  />
                  <span>
                    {syncing
                      ? account.status === "syncing" && account.sync_folder
                        ? `${account.sync_folder}: ${account.sync_cached.toLocaleString()} of ${account.sync_total.toLocaleString()}`
                        : "Connecting to your mail server…"
                      : (account.error ??
                        (account.last_synced
                          ? `Last synced ${new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(account.last_synced * 1000)}`
                          : "Ready for the first sync"))}
                  </span>
                </div>
                <div className="account-card-actions">
                  <Link
                    to="/mail"
                    search={{ account: account.id, view: "inbox" }}
                    className="button button-secondary"
                    aria-label={`Open ${account.config.name} inbox`}
                  >
                    Open inbox
                    <Icon name="chevron" size={14} />
                  </Link>
                  {syncing ? (
                    <button
                      type="button"
                      className="button button-quiet"
                      onClick={() => {
                        void mail.cancelSync(account.id);
                      }}
                    >
                      Stop sync
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="button button-quiet"
                      onClick={() => {
                        void mail.sync(account.id);
                      }}
                    >
                      <Icon name="refresh" size={14} />
                      Sync now
                    </button>
                  )}
                </div>
              </article>
            );
          })}
        </div>
        <div className="overview-connected-footer">
          <Icon name="lock" size={16} />
          <p>
            Your mail and search index are encrypted on this device. Sending and
            mailbox changes are not available in this read-only build.
          </p>
        </div>
        <ManageAccount account={managed} onDismiss={() => setManaged(null)} />
      </section>
    );
  }
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
            onClick={mail.connect}
            disabled={!mail.desktop || mail.loading}
          >
            Connect an account
            <Icon name="chevron" size={15} />
          </button>
          <span className="welcome-footnote">
            {mail.desktop
              ? "IMAP with TLS or STARTTLS. Stored in your keyring."
              : "Open the desktop app to connect your accounts."}
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
              <dd>{mail.desktop ? "Ready to connect" : "Desktop app"}</dd>
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
          Read and search mail. Sending and organisation are coming next.{" "}
          <button type="button" onClick={openAbout}>
            What’s available?
          </button>
        </p>
      </div>
    </section>
  );
}
