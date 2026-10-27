import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { MailGate, useMail } from "../components/mail-provider";
import { Modal } from "../components/modal";
import { Icon } from "../components/icon";
import { validateMailRoute } from "../lib/mail-routing";
import {
  deleteMailSearch,
  errorMessage,
  openMailMessage,
  readMailMessage,
  saveMailSearch,
  searchMail,
  type MailMessage,
  type SearchQuery,
} from "../lib/native";

export const Route = createFileRoute("/mail")({
  validateSearch: validateMailRoute,
  component: MailRoute,
});

function MailRoute() {
  const mail = useMail();
  const route = Route.useSearch();
  const saved = mail.state?.saved_searches.find(
    (saved) => saved.id === route.saved,
  );
  return (
    <MailGate>
      {route.saved && !saved ? (
        <div className="page mail-empty">
          <Icon name="search" size={30} />
          <h1>Saved search unavailable</h1>
          <p>This saved search may have been removed.</p>
        </div>
      ) : (
        <Mailbox
          key={route.saved ?? "ad-hoc"}
          initialQuery={saved?.query}
          savedName={saved?.name}
        />
      )}
    </MailGate>
  );
}

function Mailbox({
  initialQuery,
  savedName,
}: {
  initialQuery?: SearchQuery;
  savedName?: string;
}) {
  const mail = useMail();
  const route = Route.useSearch();
  const navigate = Route.useNavigate();
  const [text, setText] = useState(initialQuery?.text ?? "");
  const [debounced, setDebounced] = useState(text);
  const [unread, setUnread] = useState(initialQuery?.unread_only ?? false);
  const [attachments, setAttachments] = useState(
    initialQuery?.has_attachments ?? false,
  );
  const [junk, setJunk] = useState(initialQuery?.include_junk_trash ?? false);
  const [page, setPage] = useState(0);
  const [selected, setSelected] = useState<number | null>(null);
  const [saveOpen, setSaveOpen] = useState(false);
  const [searchName, setSearchName] = useState("");
  const searchInput = useRef<HTMLInputElement>(null);
  const accountId =
    route.account === "all"
      ? undefined
      : (route.account ?? initialQuery?.account_id ?? undefined);
  const account = mail.state?.accounts.find(
    (account) => account.id === accountId,
  );
  const routeFolder = mail.state?.folders.find(
    (folder) => folder.id === route.folder,
  );
  const folder =
    route.folder === "all" || route.view === "all"
      ? undefined
      : (routeFolder?.name ??
        (route.view === "inbox"
          ? "INBOX"
          : (initialQuery?.folder ?? undefined)));
  const query: SearchQuery = {
    text: debounced,
    account_id: accountId,
    folder,
    unread_only: unread,
    has_attachments: attachments,
    include_junk_trash: junk,
    limit: 50,
    offset: page * 50,
  };
  const results = useQuery({
    queryKey: ["mail", "search", query],
    queryFn: ({ signal }) => searchMail(query, signal),
    enabled: mail.desktop && !!mail.state,
    refetchInterval: mail.active.size > 0 || mail.state?.accounts.some((item) => item.status === "syncing") ? 2000 : 30000,
  });
  const detail = useQuery({
    queryKey: ["mail", "message", selected],
    queryFn: () => readMailMessage(selected!),
    enabled: mail.desktop && !!mail.state && selected !== null,
    // A remote deletion can remove this local row. Refresh the selected body
    // with background sync and discard it when leaving the reader.
    refetchInterval: 5000,
    gcTime: 0,
  });
  const formatted = useMutation({
    mutationFn: () => openMailMessage(selected!),
  });
  const save = useMutation({
    mutationFn: () => saveMailSearch(searchName, { ...query, text }),
    onSuccess: async (saved) => {
      await mail.refresh();
      setSaveOpen(false);
      setSearchName("");
      await navigate({ search: { saved: saved.id } });
    },
  });
  const removeSaved = useMutation({
    mutationFn: () => deleteMailSearch(route.saved!),
    onSuccess: async () => {
      await mail.refresh();
      await navigate({ search: {} });
    },
  });
  const scopedAccounts =
    mail.state?.accounts.filter(
      (item) => !accountId || item.id === accountId,
    ) ?? [];
  const syncing = scopedAccounts.filter(
    (item) => item.status === "syncing" || mail.active.has(item.id),
  );
  const failures = scopedAccounts.filter(
    (item) => item.status === "error" || item.status === "interrupted",
  );
  const total = results.data?.total ?? 0;
  const messages = results.data?.messages ?? [];
  useEffect(() => {
    const timeout = setTimeout(() => setDebounced(text), 200);
    return () => clearTimeout(timeout);
  }, [text]);
  useEffect(() => {
    setPage(0);
    setSelected(null);
  }, [debounced, accountId, folder, unread, attachments, junk]);
  useEffect(() => {
    searchInput.current?.focus();
  }, []);
  const folderOptions =
    mail.state?.folders.filter(
      (item) => !accountId || item.account_id === accountId,
    ) ?? [];
  const title =
    savedName ??
    account?.config.name ??
    (folder === "INBOX" ? "All inboxes" : "All mail");

  return (
    <section
      className={`mail-page${selected !== null ? " message-selected" : ""}`}
    >
      <header className="mail-heading">
        <div>
          <h1>{title}</h1>
          <p>
            {account ? account.config.address : "Your accounts, together"}
            {folder ? ` / ${folder}` : " / All folders"}
          </p>
        </div>
        <div className="inline-actions">
          <span
            className="read-only-pill"
            title="Opening a message does not change its server read status in this build."
          >
            <Icon name="lock" size={12} />
            Read-only
          </span>
          <button
            type="button"
            className="button button-secondary"
            disabled={!scopedAccounts.length || !!syncing.length}
            onClick={() => {
              for (const account of scopedAccounts) void mail.sync(account.id);
            }}
          >
            <Icon
              name="refresh"
              size={14}
              className={syncing.length ? "spinning" : undefined}
            />
            Refresh mail
          </button>
        </div>
      </header>
      <div className="mail-search">
        <Icon name="search" size={19} />
        <input
          ref={searchInput}
          aria-label="Search mail"
          value={text}
          onChange={(event) => setText(event.target.value)}
          placeholder="Search your mail…"
          spellCheck={false}
          maxLength={4096}
        />
        {text && (
          <button
            type="button"
            className="icon-button"
            aria-label="Clear search"
            onClick={() => setText("")}
          >
            <Icon name="close" size={15} />
          </button>
        )}
        <button
          type="button"
          className="button button-quiet"
          disabled={!mail.state?.accounts.length || results.isError}
          onClick={() => {
            setSaveOpen(true);
            save.reset();
          }}
        >
          Save search
        </button>
      </div>
      <div className="mail-filters">
        <select
          aria-label="Filter by account"
          value={accountId ?? "all"}
          onChange={(event) => {
            void navigate({
              search: (previous) => ({
                ...previous,
                account: event.target.value,
                folder: "all",
                view: "all",
              }),
            });
          }}
        >
          <option value="all">All accounts</option>
          {mail.state?.accounts.map((account) => (
            <option key={account.id} value={account.id}>
              {account.config.name}
            </option>
          ))}
        </select>
        <select
          aria-label="Filter by folder"
          value={
            routeFolder?.id ??
            (folder === "INBOX"
              ? "inbox"
              : (folderOptions.find((item) => item.name === folder)?.id ??
                "all"))
          }
          onChange={(event) => {
            void navigate({
              search: (previous) => ({
                ...previous,
                account: folderOptions.find((item) => item.id === event.target.value)?.account_id ?? previous.account,
                folder:
                  event.target.value === "inbox"
                    ? undefined
                    : event.target.value,
                view: event.target.value === "inbox" ? "inbox" : undefined,
              }),
            });
          }}
        >
          <option value="all">All folders</option>
          <option value="inbox">All inboxes</option>
          {folderOptions.map((item) => (
            <option key={item.id} value={item.id}>
              {accountId
                ? item.name
                : `${mail.state?.accounts.find((account) => account.id === item.account_id)?.config.name} / ${item.name}`}
            </option>
          ))}
        </select>
        <label>
          <input
            type="checkbox"
            checked={unread}
            onChange={(event) => setUnread(event.target.checked)}
          />
          Unread
        </label>
        <label>
          <input
            type="checkbox"
            checked={attachments}
            onChange={(event) => setAttachments(event.target.checked)}
          />
          Attachments
        </label>
        <label>
          <input
            type="checkbox"
            checked={junk}
            onChange={(event) => setJunk(event.target.checked)}
          />
          Include junk &amp; trash
        </label>
      </div>
      <details className="mail-search-tips">
        <summary>Search tips</summary>
        <p>
          Combine words and phrases with <code>from:alice</code>,{" "}
          <code>to:team</code>, <code>subject:invoice</code>,{" "}
          <code>folder:Archive</code>, <code>account:Personal</code>,{" "}
          <code>after:2026-01-01</code>, <code>before:2026-12-31</code>,{" "}
          <code>is:unread</code>, or <code>has:attachment</code>. Dates use this
          device’s timezone.
        </p>
      </details>
      {!!syncing.length && (
        <div className="sync-coverage" role="status">
          <Icon name="refresh" size={13} className="spinning" />
          <span>
            Syncing {syncing[0]!.config.name}
            {syncing[0]!.sync_folder
              ? ` / ${syncing[0]!.sync_folder} · ${syncing[0]!.sync_cached.toLocaleString()} of ${syncing[0]!.sync_total.toLocaleString()}`
              : "…"}
            . Search includes downloaded mail.
          </span>
          <button
            type="button"
            onClick={() => {
              for (const account of syncing) void mail.cancelSync(account.id);
            }}
          >
            Stop sync
          </button>
        </div>
      )}
      {!syncing.length && !!failures.length && (
        <div className="sync-coverage sync-warning">
          <Icon name="alert" size={14} />
          <span>
            Sync needs attention. Cached mail is still available; some server
            messages may not be downloaded.
          </span>
        </div>
      )}
      <div className="mail-split">
        <div className="message-list-panel">
          <div className="message-list-heading">
            <span>
              {results.isFetching
                ? "Searching…"
                : `${total.toLocaleString()} ${total === 1 ? "message" : "messages"}`}
            </span>
            {savedName && (
              <button
                type="button"
                className="icon-button"
                aria-label="Delete saved search"
                title="Delete saved search"
                onClick={() => removeSaved.mutate()}
                disabled={removeSaved.isPending}
              >
                <Icon name="trash" size={14} />
              </button>
            )}
          </div>
          {results.isError ? (
            <div className="list-empty" role="alert">
              <Icon name="alert" size={24} />
              <h2>Search could not finish</h2>
              <p>{errorMessage(results.error)}</p>
              <button
                className="button button-secondary"
                type="button"
                onClick={() => {
                  void results.refetch();
                }}
              >
                Try again
              </button>
            </div>
          ) : results.isPending ? (
            <div className="list-empty">
              <Icon name="refresh" className="spinning" size={24} />
              <p>Searching the local index…</p>
            </div>
          ) : !messages.length ? (
            <div className="list-empty">
              <Icon name="inbox" size={30} />
              <h2>
                {text || unread || attachments
                  ? "No matching mail"
                  : "No mail here yet"}
              </h2>
              <p>
                {mail.state?.accounts.length
                  ? "Try another search or refresh your accounts."
                  : "Connect an account to start receiving mail."}
              </p>
              {!mail.state?.accounts.length && (
                <button
                  className="button button-primary"
                  type="button"
                  onClick={mail.connect}
                >
                  Connect an account
                </button>
              )}
            </div>
          ) : (
            <div
              className="message-list"
              role="group"
              aria-label="Messages"
              onKeyDown={(event) => {
                if (!["ArrowDown", "ArrowUp"].includes(event.key)) return;
                event.preventDefault();
                const current = messages.findIndex(
                  (message) => message.id === selected,
                );
                const index = Math.max(
                  0,
                  Math.min(
                    messages.length - 1,
                    current + (event.key === "ArrowDown" ? 1 : -1),
                  ),
                );
                const message = messages[index];
                if (message) {
                  setSelected(message.id);
                  document.getElementById(`message-${message.id}`)?.focus();
                }
              }}
            >
              {messages.map((message) => (
                <button
                  type="button"
                  id={`message-${message.id}`}
                  key={message.id}
                  className={`message-row${selected === message.id ? " selected" : ""}${message.unread ? " unread" : ""}`}
                  aria-label={`${message.subject} — ${message.sender}`}
                  aria-pressed={selected === message.id}
                  onClick={() => {
                    setSelected(message.id);
                    formatted.reset();
                  }}
                >
                  <span className="message-dot" />
                  <span className="message-row-content">
                    <span className="message-row-top">
                      <strong>{message.sender || "Unknown sender"}</strong>
                      <time
                        dateTime={new Date(message.date * 1000).toISOString()}
                      >
                        {new Intl.DateTimeFormat(undefined, {
                          month: "short",
                          day: "numeric",
                        }).format(message.date * 1000)}
                      </time>
                    </span>
                    <span className="message-subject">
                      {message.subject || "(No subject)"}
                      {message.has_attachments && (
                        <Icon name="file" size={12} />
                      )}
                    </span>
                    <span className="message-preview">
                      {message.preview || "No text preview"}
                    </span>
                    {!accountId && (
                      <span className="message-account">
                        {
                          mail.state?.accounts.find(
                            (account) => account.id === message.account_id,
                          )?.config.name
                        }{" "}
                        / {message.folder}
                      </span>
                    )}
                  </span>
                </button>
              ))}
            </div>
          )}
          <div className="mail-pagination">
            <button
              type="button"
              aria-label="Previous page"
              disabled={page === 0 || results.isFetching}
              onClick={() => {
                setPage(page - 1);
                setSelected(null);
              }}
            >
              <Icon name="chevron" size={14} className="chevron-back" />
            </button>
            <span>
              {total
                ? `${page * 50 + 1}–${Math.min((page + 1) * 50, total)} of ${total.toLocaleString()}`
                : "0 messages"}
            </span>
            <button
              type="button"
              aria-label="Next page"
              disabled={(page + 1) * 50 >= total || results.isFetching}
              onClick={() => {
                setPage(page + 1);
                setSelected(null);
              }}
            >
              <Icon name="chevron" size={14} />
            </button>
          </div>
        </div>
        <article className="message-reader" aria-label="Message reader">
          {selected === null ? (
            <div className="reader-empty">
              <Icon name="mail" size={36} />
              <h2>A little room to read.</h2>
              <p>Select a message to open it here.</p>
            </div>
          ) : detail.isPending ? (
            <div className="reader-empty">
              <Icon name="refresh" size={24} className="spinning" />
              <p>Opening cached message…</p>
            </div>
          ) : detail.isError ? (
            <div className="reader-empty" role="alert">
              <Icon name="alert" size={26} />
              <h2>Message unavailable</h2>
              <p>{errorMessage(detail.error)}</p>
              <button
                type="button"
                className="button button-secondary"
                onClick={() => setSelected(null)}
              >
                Back to messages
              </button>
            </div>
          ) : detail.data ? (
            <MessageReader
              message={detail.data}
              onBack={() => setSelected(null)}
              onFormatted={() => formatted.mutate()}
              formattedPending={formatted.isPending}
              formattedError={formatted.error}
            />
          ) : null}
        </article>
      </div>
      {removeSaved.isError && (
        <p className="mail-inline-error" role="alert">
          {errorMessage(removeSaved.error)}
        </p>
      )}
      <Modal
        open={saveOpen}
        onDismiss={() => setSaveOpen(false)}
        title="Save search"
        className="form-dialog"
        dismissible={!save.isPending}
      >
        <h2>Save search</h2>
        <p>Keep these filters in your encrypted mail store.</p>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            save.mutate();
          }}
        >
          <label htmlFor="saved-search-name">
            Search name
            <input
              id="saved-search-name"
              value={searchName}
              onChange={(event) => setSearchName(event.target.value)}
              maxLength={100}
              required
            />
          </label>
          {save.isError && <p role="alert">{errorMessage(save.error)}</p>}
          <div className="modal-actions">
            <button
              type="button"
              className="button button-quiet"
              onClick={() => setSaveOpen(false)}
              disabled={save.isPending}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="button button-primary"
              disabled={save.isPending}
            >
              {save.isPending ? "Saving…" : "Save"}
            </button>
          </div>
        </form>
      </Modal>
    </section>
  );
}

function MessageReader({
  message,
  onBack,
  onFormatted,
  formattedPending,
  formattedError,
}: {
  message: MailMessage;
  onBack: () => void;
  onFormatted: () => void;
  formattedPending: boolean;
  formattedError: unknown;
}) {
  return (
    <>
      <div className="reader-toolbar">
        <button
          className="button button-quiet reader-back"
          type="button"
          onClick={onBack}
        >
          <Icon name="chevron" size={14} className="chevron-back" />
          Messages
        </button>
        {message.has_html && (
          <button
            className="button button-secondary"
            type="button"
            onClick={onFormatted}
            disabled={formattedPending}
          >
            {formattedPending ? "Opening…" : "Open formatted view"}
          </button>
        )}
        <span>Plain text</span>
      </div>
      <div className="reader-content">
        <h2>{message.summary.subject || "(No subject)"}</h2>
        <div className="reader-identity">
          <span className="sender-avatar">
            {(message.summary.sender || "?").slice(0, 1).toUpperCase()}
          </span>
          <div>
            <strong>{message.summary.sender || "Unknown sender"}</strong>
            <p>To: {message.summary.recipients}</p>
            <time>
              {new Intl.DateTimeFormat(undefined, {
                dateStyle: "medium",
                timeStyle: "short",
              }).format(message.summary.date * 1000)}
            </time>
          </div>
        </div>
        {formattedError != null && (
          <p role="alert" className="mail-inline-error">
            {errorMessage(formattedError)}
          </p>
        )}
        <div className="message-text">
          {message.body_text || "This message has no text body."}
        </div>
        {!!message.attachments.length && (
          <section className="message-attachments">
            <h3>
              {message.attachments.length}{" "}
              {message.attachments.length === 1 ? "attachment" : "attachments"}
            </h3>
            {message.attachments.map((attachment) => (
              <div key={attachment.part}>
                <Icon name="file" size={20} />
                <span>
                  {attachment.filename}
                  <small>{attachment.content_type}</small>
                </span>
              </div>
            ))}
            <p>
              Attachment downloads and embedded images are coming in a later
              build.
            </p>
          </section>
        )}
      </div>
    </>
  );
}
