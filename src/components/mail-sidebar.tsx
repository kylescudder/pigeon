import { Link, useRouterState } from "@tanstack/react-router";
import { Icon, type IconName } from "./icon";
import { useMail } from "./mail-provider";

export function folderIcon(kind: string): IconName {
  return kind === "inbox"
    ? "inbox"
    : kind === "archive"
      ? "archive"
      : kind === "trash"
        ? "trash"
        : kind === "junk"
          ? "shield"
          : "mail";
}

export function MailSidebar() {
  const mail = useMail();
  const accountId = useRouterState({
    select: (state) => (state.location.search as { account?: string }).account,
  });
  const folderId = useRouterState({
    select: (state) => (state.location.search as { folder?: string }).folder,
  });
  return (
    <div className="mail-sidebar">
      <div className="sidebar-section-heading">
        <span className="section-label">Accounts</span>
        <button
          className="icon-button"
          aria-label="Connect account"
          title="Connect account"
          type="button"
          onClick={mail.connect}
          disabled={!mail.desktop || !mail.state}
        >
          <Icon name="plus" size={14} />
        </button>
      </div>
      {mail.state?.accounts.length ? (
        mail.state.accounts.map((account, index) => {
          const folders = mail.state!.folders.filter(
            (folder) => folder.account_id === account.id,
          );
          const unread = folders
            .filter((folder) => folder.kind === "inbox")
            .reduce((total, folder) => total + folder.unread, 0);
          return (
            <div className="sidebar-account" key={account.id}>
              <Link
                to="/mail"
                search={{ account: account.id, view: "inbox" }}
                className={`account-link${accountId === account.id ? " selected" : ""}`}
                title={account.config.name}
              >
                <span className={`account-avatar avatar-${index % 4}`}>
                  {account.config.name.slice(0, 1).toUpperCase()}
                </span>
                <span className="sidebar-label account-link-name">
                  {account.config.name}
                </span>
                {account.status === "syncing" ? (
                  <Icon
                    name="refresh"
                    size={12}
                    className="spinning account-unread"
                  />
                ) : unread > 0 ? (
                  <span className="account-unread sidebar-label">
                    {unread.toLocaleString()}
                  </span>
                ) : null}
              </Link>
              {accountId === account.id && (
                <div className="sidebar-folders">
                  {folders.map((folder) => (
                    <Link
                      to="/mail"
                      key={folder.id}
                      search={{ account: account.id, folder: folder.id }}
                      className={`folder-link${folderId === folder.id ? " selected" : ""}`}
                      title={folder.name}
                    >
                      <Icon name={folderIcon(folder.kind)} size={14} />
                      <span>{folder.name}</span>
                      {folder.unread > 0 && <small>{folder.unread}</small>}
                    </Link>
                  ))}
                </div>
              )}
            </div>
          );
        })
      ) : (
        <div className="sidebar-account-empty">
          <span className="account-placeholder">
            <Icon name="mail" size={16} />
          </span>
          <div>
            {mail.error ? "Mail store unavailable" : "No accounts yet"}
            <span>
              {mail.error
                ? "Unlock the store to continue."
                : mail.desktop
                ? "Connect your first account."
                : "Open the desktop app to connect."}
            </span>
          </div>
        </div>
      )}
      {!!mail.state?.saved_searches.length && (
        <div className="saved-searches">
          <span className="section-label">Saved searches</span>
          {mail.state.saved_searches.map((saved) => (
            <Link
              to="/mail"
              search={{ saved: saved.id }}
              className="saved-search-link"
              key={saved.id}
              title={saved.name}
            >
              <Icon name="search" size={14} />
              <span className="sidebar-label">{saved.name}</span>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
