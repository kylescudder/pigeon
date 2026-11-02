import { Link, useRouterState } from "@tanstack/react-router";
import { useState } from "react";
import {
  ancestorPaths,
  buildFolderTree,
  subtreeUnread,
  type FolderNode,
} from "../lib/folder-tree";
import type { MailFolder } from "../lib/native";
import { ManageFolders } from "./manage-folders";
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
                <ManageFolders
                  account={account}
                  folders={folders}
                  viewing={folderId}
                />
              )}
              {accountId === account.id && (
                <div className="sidebar-folders">
                  <div role="tree">
                    <FolderTree
                      accountId={account.id}
                      folders={folders}
                      selected={folderId}
                    />
                  </div>
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
      {mail.state?.managed_warnings?.map((warning) => (
        <p role="alert" className="mail-inline-error" key={warning}>
          {warning}
        </p>
      ))}
      <PendingLink />
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

/** Shown while server changes are outstanding; warns when any need attention. */
function PendingLink() {
  const mail = useMail();
  const counts = mail.state?.actions;
  const total = counts ? counts.pending + counts.failed + counts.conflict : 0;
  if (!total) return null;
  const attention = counts!.failed + counts!.conflict;
  return (
    <Link
      to="/pending"
      className={`saved-search-link pending-link${attention ? " needs-attention" : ""}`}
      title={
        attention
          ? `${attention} change${attention === 1 ? "" : "s"} need attention`
          : "Changes waiting for the mail server"
      }
    >
      <Icon name={attention ? "alert" : "refresh"} size={14} />
      <span className="sidebar-label">Pending changes</span>
      <span className="pending-count">{total.toLocaleString()}</span>
    </Link>
  );
}

function FolderTree({
  accountId,
  folders,
  selected,
}: {
  accountId: string;
  folders: MailFolder[];
  selected: string | undefined;
}) {
  const tree = buildFolderTree(folders);
  // In memory only: folder names never go to persistent browser storage.
  const [toggled, setToggled] = useState(new Map<string, boolean>());
  const opened = new Set(ancestorPaths(tree, selected));
  const isOpen = (path: string) => toggled.get(path) ?? opened.has(path);
  const toggle = (path: string) =>
    setToggled((previous) => new Map(previous).set(path, !isOpen(path)));
  const render = (node: FolderNode, depth: number) => {
    const open = node.children.length > 0 && isOpen(node.path);
    const unread = open ? (node.folder?.unread ?? 0) : subtreeUnread(node);
    const content = (
      <>
        <Icon name={folderIcon(node.folder?.kind ?? "other")} size={14} />
        <span>{node.label}</span>
        {unread > 0 && <small>{unread.toLocaleString()}</small>}
      </>
    );
    return (
      <div
        key={node.path}
        role="treeitem"
        aria-expanded={node.children.length ? open : undefined}
        aria-selected={!!node.folder && node.folder.id === selected}
      >
        <div className="folder-row" style={{ paddingLeft: depth * 12 }}>
          {node.children.length ? (
            <button
              type="button"
              className={`folder-toggle${open ? " open" : ""}`}
              aria-label={`${open ? "Collapse" : "Expand"} ${node.label}`}
              onClick={() => toggle(node.path)}
            >
              <Icon name="chevron" size={11} />
            </button>
          ) : (
            <span className="folder-toggle" />
          )}
          {node.folder ? (
            <Link
              to="/mail"
              search={{ account: accountId, folder: node.folder.id }}
              className={`folder-link${node.folder.id === selected ? " selected" : ""}`}
              title={node.folder.name}
            >
              {content}
            </Link>
          ) : (
            <span className="folder-link folder-placeholder" title={node.path}>
              {content}
            </span>
          )}
        </div>
        {open && (
          <div role="group">
            {node.children.map((child) => render(child, depth + 1))}
          </div>
        )}
      </div>
    );
  };
  return <>{tree.map((node) => render(node, 0))}</>;
}
